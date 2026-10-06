"""tier_3 e2e: the onboarding **launch-routing** smoke cases — CROSS-APP.

These automate the linux onboarding smoke-test cases (A-I; internal dev note),
which were historically MANUAL. The reason they were manual is narrow and now
fixable: every e2e driver isolates each launch with a *fresh per-launch*
``FAUNA_KEYRING_APP=fauna-e2e-agent-<port>`` namespace **and** a *fresh
per-launch* ``FAUNA_E2E_CREDENTIAL_DIR`` tmpdir, so neither the identity rows nor
the pending-invite slot survives a force-quit + relaunch — exactly the
persistence these cases hinge on.

The fix is the shared **launch harness** (``tests/common/launch_harness.py``):
one interface over the two launch/relaunch lifecycles, so a single module drives
native and web without per-app copies (priority #1). The harness owns the
persistent-credential adapter (``tests/common/cred_store.py``) each app reads
its identity back through on relaunch:

* **linux** rides the driver's ``use_real_keyring`` mode — a STABLE keyring
  namespace + STABLE XDG dirs on a real Secret Service, so every slot (identity
  rows + pin-store) persists. That service is a ``gnome-keyring-daemon`` the
  test's ``LibsecretCredStore`` runs privately on the launch's own bus (never
  the desktop keyring), so the gate is only "is the daemon installed".
  **Exception: case A** (pending-invite) uses the file-backed store instead —
  the shared ``AccountRegistry`` slot it lives on (``libs/fauna-client-accounts``
  over ``fauna-credential-store``) honors ``FAUNA_E2E_CREDENTIAL_DIR``
  independently, so it needs no daemon at all, like tui, and is marked
  ``headless_credential_store`` to skip the module's real-keyring gate.
* **tui** pins the shared store's ``FAUNA_E2E_CREDENTIAL_DIR`` file backend
  (``libs/fauna-credential-store``) to a stable tmpdir — the documented fallback
  for keyring-unavailable boxes. Headless-safe, no D-Bus, no skip.
* **macos / ios** have no real-keyring equivalent at all, so they always ride
  the same file backend as tui (``AppleFileCredStore``, ``tests/common/
  cred_store.py``) — a stable ``credential_dir`` the app's own
  ``KeychainStore.e2eFileURL`` E2E gate reads back
  (``drivers/macos.py``/``drivers/ios.py`` ``launch()``, built with this exact
  parity in mind). Headless-safe, no skip. Only in ``NATIVE_ONLY`` — the
  seed-and-launch cases (B/C/D/E/F) stay unjoined for now (no test-authored
  case yet exercises them on apple).
* **windows** rides the same file backend as tui/apple (``WindowsFileCredStore``,
  ``tests/common/cred_store.py``): the C# ``FileSecretBackend`` is a deliberate
  twin of linux's file store, selected by the same ``FAUNA_E2E_CREDENTIAL_DIR``
  the driver already sets, and there is no real-Credential-Manager mode to fall
  back to (the suite must never touch the dev box's real store). Headless-safe,
  no skip. ``SecretKeyMap`` remaps nothing — the adapter keys
  every logical key verbatim, the same as linux/tui.
* **android** has no host-side path to its store at all: the on-device bridge
  writes the identity into the app's own ``filesDir`` on the ``/session`` POST
  (``AndroidCredStore``), and a relaunch is a force-stop + start that does not
  re-send it (``AndroidLaunchHarness``). A case's launch env crosses only by
  names the bridge carries into ``MainActivity``'s ``Os.setenv`` door, and
  anything else is refused. Joined 2026-09-25 to B/C/D/E/F (``WEB_FIT``) —
  these cases only ever call ``launch_and_route`` (a plain ``launch()`` for
  native), the exact same call shape case L already proved on android; no new
  harness code was needed, only the client-set add.
* **web** has no external backend and no ``app_path``: the identity lives in the
  SPA origin's ``localStorage``, seeded on the running browser, and a "relaunch"
  is a ``hard_reload()``. It joins the **seed-and-launch routing** cases — those
  whose whole assertion is where the shared ``LaunchMachine`` routes a stored
  identity on launch (B/C/D/E/F). The launch machine dials the injected
  ``node_url`` cross-origin over WS, so a web browser reaches an unreachable /
  unregistered / registered nest exactly as a native binary does; the retry /
  invite / handle / feed surfaces all render on web. Web is NOT in the cases that
  don't fit its lifecycle — see the per-case notes below.

All supported clients drive the same launch machine over the same routing table,
so this is ONE module parametrized by client rather than a per-app copy
(priority #1). The silent challenge itself runs unmocked
(``run_pinned_silent_challenge`` has no e2e gating), so these exercise the genuine
routing.

**Per-case client fit** (indirect-parametrized `launch_harness`; each test names
the client set it targets and why):

* **B, C, D, E, F — all apps incl. web.** Seed a stored identity, run the
  launch path (``launch_and_route``), assert the routed surface. The web
  ``LaunchMachine`` connects to the injected per-actor ``nest_url`` itself, so web
  reaches the same routing landings the native binaries do (recon 2026-07-15).
* **G (factory reset) — native only.** Web's ``driver.reset()`` sweeps the whole
  ``fauna_*``/``fauna/*`` localStorage namespace as the *test agent's* cleanup
  (``web-bridge/agent.js``), not through the product path, so a reset-erase
  assertion would green on the harness and pin nothing; ``WebCredStore``
  deliberately leaves ``stored_accounts()`` unimplemented, and a browser
  ``teardown()`` discards the whole profile so there is no force-quit analogue.
  Structural, not a gap.
* **A (pending invite survives force-quit) — native only.** The real submit +
  persist + rehydrate is already covered on web by
  ``tests/web/test_pending_invite_persistence.py`` +
  ``tests/web/test_pending_invite_recheck_404.py``; joining here would duplicate
  it, and the case is bound to the native file-backed cred store the web adapter
  has no analogue for. Priority #4 — don't replicate covered behavior on a 7th
  surface.
* **I (deferred DNS → "Almost ready") — native only.** Already covered on web by
  ``tests/web/test_awaiting_dns_persistence.py`` (both relaunch-hydration AND the
  real ``dns_post_instructions`` exit). Joining would duplicate it.
* **J (`--autostart` tray residency) — ``AUTOSTART_APPS`` (windows today).**
  The only case whose subject is the launch's *side effect on the desktop* rather
  than the surface it routes to: the same routing decision that picks a surface
  also decides whether a window opens at all. It belongs here because it IS a
  launch-routing assertion driven by the same seeded-identity-vs-nothing-stored
  contrast every other case uses — and the set is a set, not a windows-only
  module, so linux's entrusted leg joins by adding a name.

* **K (a real onboarding completion reaches the main app) — ``NATIVE_ONLY``.**
  The one case that finishes the WIZARD instead of seeding a store: it runs the
  handle check against a live nest, so it is the only proof here that the
  post-wizard hand-off works at all. Native because the typed
  ``<localpart>@127.0.0.1:<port>`` handle resolves to an origin the check dials
  DIRECTLY, which a browser cannot (web reaches nests through the CORS
  ``spa_url`` proxy — a different origin than the handle resolves to). Verified
  green on **windows** 2026-08-10 (the app that had no such proof at all); the other four arms run the same shared machine and
  the same journey `test_mail_auto_enable_first_setup.py` already drives green on
  linux/macOS/tui, so a red on one of them is a finding for that app, not an
  expected gap.

Authoritative behavior: ``docs/goal/behavior/onboarding.md`` § App-launch
routing — silent-challenge fallback table (the 5 outcomes Online / NotRegistered
{claimed→invite_request | unclaimed→claim_code} / Transient→retry-surface /
NeedsUpdate→non-retry / SecretInvalid) and the startup branch
(``libs/fauna-launch-machine/src/machine.rs::start`` — identity+pending,no-url →
invite_request; identity,no-url,no-pending → handle_entry). NOTE: an *unreachable*
nest (DNS-fail OR connection-refused) routes to the **retry surface**, not
auto-handle_entry; handle_entry is reached only via the retry surface's
``launch-fallthrough-button`` ("Use a different nest"). The procedure doc's older
"case D = unreachable → handle_entry" predated this table and is corrected there.

Case G has no manual-doc ancestor: it pins that a factory ``reset()`` empties the
credential namespace, so the *next* launch takes the routing table's
"nothing stored" row. It lives here because that is a launch-routing assertion
and it reuses this module's persistent-namespace harness.

Case H — RETIRED (no-modes, ratified 2026-07-12): it used to pin the
**admin-claimed, mode-unresolved** launch-routing row (an admin who claims a
nest and defers the storage-mode choice must be routed back to
`encryption_mode_choice` on the next launch). That row is explicitly retired
as a native wizard route (`docs/goal/behavior/onboarding.md` § App-launch
routing, "Admin-claimed, mode-unresolved — RETIRED as a native wizard
route") — every nest is content-ready from first boot now, so there is no
more claimed-but-mode-unresolved window to strand in or route back to. See
the retirement note at the former case H's location below.

`nest_instance` builds a real fauna-nest binary (tier_3).
``docs/goal/behavior/onboarding.md`` § Implementation status records this module
as the automated coverage.
"""

from __future__ import annotations

import contextlib
import json
import secrets
import socket
import time

import pytest
from nacl.signing import SigningKey

from common.auth import register_handled_actor
from common.cred_store import INSTALL_DEVICE_SECRET_SLOT, requires_secret_service
from common.keyring import secret_service_available
from helpers.app_surface import app_name, skip_environment, skip_unbuilt
from common.launch_harness import make_launch_harness, reached_authenticated_app
from i18n.strings import S
from conftest import _trust_seeder, get_available_apps
from helpers.waiting import await_account_runtime_assembled, wait_until

pytestmark = [
    pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui, pytest.mark.web,
    pytest.mark.macos, pytest.mark.ios, pytest.mark.windows, pytest.mark.android,
]

# Element IDs (tests/e2e-unified/ui.yaml § onboarding + launch).
HANDLE_INPUT = "handle-input"
INVITE_SUBMIT = "invite-request-submit-button"
INVITE_RECHECK = "invite-request-recheck-button"
INVITE_STATUS = "invite-request-status"
LAUNCH_TRANSIENT_ERROR = "launch-transient-error"
LAUNCH_RETRY = "launch-retry-button"
LAUNCH_FALLTHROUGH = "launch-fallthrough-button"
# The sign-in-refused surface's own text element (ui.yaml
# `onboarding.launch_sign_in_refused`), so smoke E cannot be satisfied by any
# old `error-message` text.
SIGN_IN_REFUSED_NOTICE = "launch-sign-in-refused-notice"
# The apps that paint that notice. The others still carry the sentence in
# `error-message` until they adopt the element.
_SIGN_IN_REFUSED_NOTICE_APPS = frozenset({"tui", "linux", "web", "macos", "ios"})
CREATE_IDENTITY = "create-identity-button"

#: Fresh web drivers compile wasm cold and the routed surface only paints after
#: the launch machine's cross-origin silent challenge settles, so the routed
#: waits use a generous ceiling. ``wait_for`` returns the instant the element
#: appears, so native is unaffected by the larger value.
ROUTE_TIMEOUT = 60


# ---------------------------------------------------------------------------
# Client sets
# ---------------------------------------------------------------------------
def _clients(*want: str) -> list[str]:
    """The wanted clients this run actually selected — intersected with whatever
    this machine / ``--client`` offers, so a bare ubuntu run stays [web, linux]
    and ``--client tui`` runs [tui]. Empty (→ no items: conftest deselects
    pytest's ``[NOTSET]`` placeholder) when none of ``want`` is available, so a
    case never runs against a client with no adapter — and the feature catalog
    reads ``want`` as the columns the case can witness
    (``features_scan.client_param_sets``)."""
    available = get_available_apps()
    return [c for c in want if c in available]


#: Cases whose flow fits every app, web included (seed-and-launch routing).
#: android joined 2026-09-25 — `launch_and_route` is a plain `launch()` on
#: native, the same shape case L already proved via `AndroidLaunchHarness`.
WEB_FIT = ("linux", "tui", "web", "android")
#: Cases that stay native (see the module docstring for the per-case reason).
#: macos/ios joined 2026-07-23, windows 2026-08-09 — all on the same file-backed
#: CredStore mechanism as tui, no per-app copy needed (priority #1).
NATIVE_ONLY = ("linux", "tui", "macos", "ios", "windows")
#: Clients that have built a tray-resident auto-start launch (case J). windows is
#: the only one TODAY — `apps/windows.md` § App Lifecycle → *Auto-start at
#: sign-in* records linux's leg (default-on autostart `.desktop` + a hidden-launch
#: equivalent) as entrusted/advisory and macOS as keeping its own login-item
#: default until its leg is decided. Kept as a SET rather than collapsing the case
#: into a windows-only module so the second client joins by adding a name here,
#: not by copying a case (priority #1).
AUTOSTART_APPS = ("windows",)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
def _await_launch_decision(driver, timeout: float = 90) -> dict:
    """Block until the app has PUBLISHED this launch's window-activation decision,
    then return it (``{"autostart": bool, "window_shown": bool}``).

    The key is ABSENT until the activation gate at the foot of the app's launch
    path has run, and the app sets it strictly AFTER calling (or deliberately
    skipping) the activation — so waiting for the key's presence is a causal
    barrier for a negative assertion, not a settle-sleep (e2e convention 14). The
    budget is generous because a green run returns on the first poll after the
    gate; only a genuine failure ever spends it."""
    state = driver.wait_for_state(
        lambda s: isinstance(s.get("launch"), dict), timeout=timeout
    )
    return state["launch"]


def _visible_windows(driver) -> list[dict]:
    """The app's on-screen top-level windows, per the OS (not per the app's own
    report). Offscreen entries are dropped: UIA lists a window that exists but was
    never shown, and "exists" is not the contract — "opened on the user's desktop"
    is."""
    return [w for w in driver.top_level_windows() if not w["is_offscreen"]]


def _free_closed_port() -> int:
    """A 127.0.0.1 port with nothing listening → `connect` gets connection-refused
    (the "nest is down" / reachable-but-refused trigger)."""
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------
@pytest.fixture
def launch_harness(request, tmp_path):
    """The launch harness for the client under test, parametrized INDIRECTLY per
    test with the client set that case fits (``request.param`` is the client).

    Native (linux/tui/macos/ios) builds a ``CredStore`` + binary harness; web
    builds the SPA-localStorage harness (which serves + launches the SPA at
    ``spa_url``). A case marked ``headless_credential_store`` gets the
    file-backed native store (and skips the keyring gate) — the pending-invite
    slot needs no real keyring; macos/ios have no keyring backend at all, so
    they always get it regardless of the marker. The client id lands in the
    test name (``[linux]``/``[tui]``/``[web]``/``[macos]``/``[ios]``), which
    conftest's ``--client`` filter reads."""
    client = request.param
    headless = request.node.get_closest_marker("headless_credential_store") is not None
    if requires_secret_service(client) and not headless and not secret_service_available():
        skip_environment(
            "linux's real-keyring launches run a private gnome-keyring-daemon, which this box cannot supply"
        )

    # `@pytest.mark.launch_args("--autostart")`: the cases whose subject is HOW the
    # binary was invoked. Read here rather than passed per-test because the fixture
    # is the only thing that constructs the harness, and read as a marker rather
    # than a second indirect param because `request.param` already carries the
    # client (the id conftest's `--client` filter reads out of the test name).
    args_marker = request.node.get_closest_marker("launch_args")
    extra: dict = {"args": " ".join(args_marker.args)} if args_marker else {}
    # `@pytest.mark.launch_env(NAME="value", ...)`: the cases whose subject is the
    # ENVIRONMENT the binary starts in (case L's wrong-clock seed). Same reasoning
    # as `launch_args` — read here because the fixture is the only constructor,
    # and a marker rather than a second indirect param.
    env_marker = request.node.get_closest_marker("launch_env")
    if env_marker:
        extra["environment"] = {k: str(v) for k, v in env_marker.kwargs.items()}
    extra = extra or None

    if client == "web":
        spa_url = request.getfixturevalue("spa_url")
        harness = make_launch_harness("web", spa_url=spa_url, extra_launch_config=extra)
    elif client == "ios":
        # No bare `ios_app_path` fixture exists — iOS's direct-launch fixture
        # (`ios_setup`, used by e.g. the account-switcher suite for the same
        # reason: a test-scoped credential seed must reach the app BEFORE
        # launch) returns `{"udid", "app_path"}` together, since
        # `drivers/ios.py`'s `launch()` requires both.
        ios_setup = request.getfixturevalue("ios_setup")
        harness = make_launch_harness(
            "ios", tmp_path=tmp_path, app_path=ios_setup["app_path"],
            udid=ios_setup["udid"], file_backed=headless, extra_launch_config=extra,
            seed_trust=_trust_seeder(request),
        )
    else:
        app_path = request.getfixturevalue(f"{client}_app_path")
        harness = make_launch_harness(
            client, tmp_path=tmp_path, app_path=app_path, file_backed=headless,
            extra_launch_config=extra, seed_trust=_trust_seeder(request),
        )
    try:
        yield harness
    finally:
        harness.teardown()


@pytest.fixture
def live_node_url(request, launch_harness):
    """The reachable nest url for the client under test — the cases that seed a
    LIVE nest (Online, unregistered-on-claimed). Native talks to the nest binary
    directly; web must reach it through the CORS ``spa_url`` proxy (a browser
    cannot dial the raw nest origin — the established ``test_nest_identity_pin.py``
    pattern; ``spa_url`` proxies the session ``nest_instance``)."""
    if launch_harness.client == "web":
        return request.getfixturevalue("spa_url")
    return request.getfixturevalue("nest_instance")["url"]


@pytest.fixture(scope="module")
def smoke_handled_nest(request, nest_mode, tmp_path_factory):
    """A nest a REAL handle check can resolve *and* dial — the session
    ``nest_instance`` is neither, which is why this fixture exists rather than
    the case reusing it.

    Two knobs, both load-bearing and both absent from ``nest_instance``
    (``conftest.py`` defaults to no domain option, ``serve_tls=False``):

    * ``handle_domain`` equal to this nest's own loopback authority, so a typed
      ``<localpart>@127.0.0.1:<port>`` handle resolves *here*
      (``account_core::handle_domain``; the same shape ``cross_nest_foreign``
      uses to make a handled actor discoverable).
    * ``serve_tls=True``, because since Pillar C (``730303718``)
      ``fauna_provisioning::probe::resolve_handle_domain`` derives **uniform
      https for any non-public authority, loopback included** — so the check
      dials ``wss://`` and a plain-HTTP nest fails the handshake. The native
      app trusts the self-signed floor through the graduated SPKI pin; no
      ``set_provider_base_urls`` override is needed or wanted (an override would
      re-introduce exactly the injection this case exists to stop relying on).

    ``open_registration`` lets :func:`register_handled_actor` seed the
    already-registered user over the wire, with no admin involved."""
    from common.auth import open_registration
    from common.nest import OWN_DIAL_AUTHORITY
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "handled-nest",
        handle_domain_seed=OWN_DIAL_AUTHORITY, serve_tls=True,
    )
    open_registration(nest)
    nest["authority"] = nest["handle_domain_seed"]
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def smoke_handled_actor(smoke_handled_nest):
    """An already-registered **non-admin** handled actor on :func:`smoke_handled_nest`.

    Non-admin on purpose: an admin's completion runs the claim path, and the
    thing case K has to prove is the ordinary new user's hand-off. Registered
    ahead of the wizard so the real handle check resolves
    ``AlreadyOnNest{handle_matches: true}`` from the nest rather than from an
    injected snapshot."""
    return register_handled_actor(
        smoke_handled_nest["port"], handle="smokearrival",
        domain=smoke_handled_nest["authority"], base_url=smoke_handled_nest["url"],
    )


# ---------------------------------------------------------------------------
# B — Online (silent-challenge success) → main app
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("launch_harness", _clients(*WEB_FIT), indirect=True)
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_b_registered_identity_relaunches_to_main_app(
    nest_instance, launch_harness, live_node_url
):
    """Authenticated-launch (smoke B's launch-routing half): a saved identity that
    the nest *does* know (`fauna.auth.verify` succeeds) routes straight to the
    authenticated main app on launch (LaunchPhase::Online → launch_authenticated),
    no wizard. We reuse the nest's own claimed-admin identity as the registered
    actor (real `/verify` success); web reaches the same nest through `spa_url`.

    No-modes retirement (ratified 2026-07-12): a nest is content-ready from
    first boot now, so the Online row is reached with no preliminary commit."""
    admin_secret_hex = bytes(nest_instance["admin"]["signing_key"]).hex()
    driver = launch_harness.launch_and_route(
        secret_hex=admin_secret_hex, node_url=live_node_url, trust=nest_instance
    )
    reached_authenticated_app(driver, timeout=90)
    assert driver.is_absent(HANDLE_INPUT), "should not be in the wizard"


# ---------------------------------------------------------------------------
# C — silent-challenge transient (nest down, connection refused) → retry surface
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("launch_harness", _clients(*WEB_FIT), indirect=True)
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_c_unreachable_refused_shows_retry_surface(launch_harness):
    """A reachable-but-refused nest (nothing listening → connection refused) is a
    transport fault the client can't reliably classify → the retry surface
    (`launch-transient-error` + Retry + "Use a different nest"), never a terminal
    drop (onboarding.md fallback table row 509). No live nest needed — the launch
    machine dials the dead url (web dials it cross-origin over WS)."""
    dead_url = f"http://127.0.0.1:{_free_closed_port()}"
    driver = launch_harness.launch_and_route(
        secret_hex=secrets.token_hex(32), node_url=dead_url, trust=None
    )
    driver.wait_for(LAUNCH_RETRY, timeout=ROUTE_TIMEOUT)
    assert driver.is_visible(LAUNCH_RETRY), "transient failure must offer Retry"
    assert driver.is_visible(LAUNCH_FALLTHROUGH), (
        "transient failure must offer 'Use a different nest'"
    )


# ---------------------------------------------------------------------------
# D — silent-challenge unreachable (DNS does not resolve) → retry surface
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("launch_harness", _clients(*WEB_FIT), indirect=True)
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_d_unreachable_dns_fail_shows_retry_surface(launch_harness):
    """A nest whose DNS does not resolve is the SAME authoritative outcome as the
    refused case (onboarding.md row 509 explicitly groups "DNS doesn't resolve"
    with the other reachability faults) → the retry surface, NOT an automatic
    handle_entry. This pins that a DNS-fail is not misclassified as terminal. The
    "use a different nest → handle_entry" path is smoke F. (Corrects the stale
    procedure-doc case D.)

    On web the `isSafeNodeUrl` guard that would reject a non-loopback `http` host
    lives only in the app-level `$lib/api.ts` path, NOT the launch machine, so the
    launch challenge still dials `ws://…invalid` → NXDOMAIN → transient."""
    bogus_url = "http://nest.smoke-does-not-resolve.invalid:8080"
    driver = launch_harness.launch_and_route(
        secret_hex=secrets.token_hex(32), node_url=bogus_url, trust=None
    )
    driver.wait_for(LAUNCH_RETRY, timeout=ROUTE_TIMEOUT)
    assert driver.is_visible(LAUNCH_RETRY), "DNS-fail must offer Retry, not drop to handle_entry"
    assert driver.is_visible(LAUNCH_FALLTHROUGH)


# ---------------------------------------------------------------------------
# E — silent-challenge not_registered on a CLAIMED nest, saved nest_url →
#     the sign-in-refused surface (never the invite wizard)
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("launch_harness", _clients(*WEB_FIT), indirect=True)
def test_smoke_e_previously_signed_in_identity_refused_by_claimed_nest_lands_refused_surface(
    nest_instance, launch_harness, live_node_url
):
    """A saved identity + saved ``nest_url`` — the store shape only a prior sign-in
    (or claim) here writes — that the *claimed* nest now refuses (`/verify` →
    `fauna.auth.not_registered`, `fauna.setup.status` → claimed) is the
    previously-signed-in row of onboarding.md § App-launch routing: the nest no
    longer signs this identity in (suspended, or removed — the app cannot tell,
    by design). It lands the sign-in-refused surface, NOT the invite wizard: the
    localized sentence in the surface's own ``launch-sign-in-refused-notice``
    and "Use a different nest"; an app that reads the snapshot's side channel
    adds Retry (tui, linux, web, macos and ios do). An app that has not adopted the notice element yet
    still paints the sentence in ``error-message``, and is recorded unbuilt
    after that interim shape is checked. Before this
    row the launch offered to re-join a nest that already held the account — a
    suspended actor's invite submit is refused outright.

    A fresh random seed stands in for the refused identity (verify answers the
    same opaque code for unregistered and suspended — no oracle); the session
    `nest_instance` (reached directly on native, through `spa_url` on web) is
    claimed. The real suspend → refused → restore → retry → online chain is
    pinned over the WS ceremony by
    ``bins/fauna-nest/tests/launch_machine_auth_roundtrip.rs``."""
    driver = launch_harness.launch_and_route(
        secret_hex=secrets.token_hex(32), node_url=live_node_url, trust=nest_instance
    )
    has_notice = app_name(driver) in _SIGN_IN_REFUSED_NOTICE_APPS
    sentence_id = SIGN_IN_REFUSED_NOTICE if has_notice else "error-message"
    driver.wait_for(sentence_id, timeout=ROUTE_TIMEOUT)
    assert driver.is_absent(INVITE_SUBMIT), (
        "a nest this app signed in to before is never re-joined through the invite wizard"
    )
    assert driver.is_visible(LAUNCH_FALLTHROUGH), (
        "the refused surface keeps 'Use a different nest'"
    )
    message = driver.get_text(sentence_id)
    assert S.onboarding.launch.sign_in_refused in (message or ""), (
        f"the refused surface paints the localized sentence in {sentence_id!r}, "
        f"got {message!r}"
    )
    if not has_notice:
        skip_unbuilt(
            driver,
            surface=SIGN_IN_REFUSED_NOTICE,
            detail="the sentence still paints in error-message (checked above)",
        )


# ---------------------------------------------------------------------------
# E2 — the same verdict MID-SESSION: suspended while signed in → the refused
#      surface with no relaunch; restore + Retry → the app again
# ---------------------------------------------------------------------------
@pytest.mark.parametrize(
    "launch_harness",
    _clients("linux", "tui", "web", "android", "macos", "ios", "windows"),
    indirect=True,
)
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_e2_suspended_while_signed_in_lands_refused_surface_without_relaunch(
    nest_instance, launch_harness, live_node_url
):
    """onboarding.md § App-launch routing, the previously-signed-in row: "The
    same verdict mid-session lands the same surface". An admin suspends a
    signed-in user through the API (`fauna.admin.users.suspend`); the nest closes
    the user's sockets with 4401 (`transport-connection.md` § Revocation
    teardown) and refuses the re-mint with `fauna.auth.not_registered`. The
    app's reconnect supervisor stops on that typed refusal, and the app routes it
    to the sign-in-refused surface — never a generic connection error, never a
    wait for the next launch. The admin's restore (`cancel_eviction`) then lets
    Retry sign the user back in, on the same process.

    Web routes the refusal from its bearer re-mint (`getAuthToken`'s typed
    `SignInRefusedError`, which also stops the wasm reconnect loop), android
    from its connection-state pump's `sessionEndingVerdict()`, macos and ios
    from FaunaKit's connection-state observer (`escalateSessionEnding`), windows
    from its connection-state pump and TTL refresh loop through
    `SessionEndingRoute`; all, like linux, reach the launch surface over the SAME
    account on the same page / process. tui, linux, web, macos and ios paint the surface's own notice +
    Retry; android and windows the interim shape (the sentence in ``error-message``) until they
    adopt the page, recorded unbuilt after that
    check. The user is registered on the session ``nest_instance`` directly;
    web reaches that same nest through ``spa_url`` (``live_node_url``)."""
    from common.auth import _authed_call, make_keypair, register_user

    admin_sk = nest_instance["admin"]["signing_key"]
    actor_hex, secret_hex = make_keypair()
    register_user(
        nest_instance["port"], actor_hex, base_url=nest_instance["url"],
        admin_signing_key=admin_sk, handle=f"refused{secrets.token_hex(3)}",
    )
    driver = launch_harness.launch_and_route(
        secret_hex=secret_hex, node_url=live_node_url, trust=nest_instance
    )
    reached_authenticated_app(driver, timeout=90)
    # The live socket the suspension must tear down: without it there is no
    # 4401 and the case would prove only a later sign-in.
    driver.wait_for_state(
        lambda s: (s.get("connection") or {}).get("online") is True, timeout=60
    )

    def admin(kind: str) -> None:
        payload = {"actor_id": bytes.fromhex(actor_hex)}
        if kind == "fauna.admin.users.suspend":
            payload |= {"category": "other", "reason": "e2e mid-session refusal"}
        _authed_call(nest_instance["url"], admin_sk, kind, payload)

    admin("fauna.admin.users.suspend")
    try:
        has_notice = app_name(driver) in _SIGN_IN_REFUSED_NOTICE_APPS
        sentence_id = SIGN_IN_REFUSED_NOTICE if has_notice else "error-message"
        # `error-message` is the generic per-page slot (convention 2): the
        # still-live authenticated window's OWN error-message can turn
        # visible first, painting a transient RPC failure (`fauna.account.get`
        # racing the suspension teardown) before the reconnect supervisor
        # tears the window down and rebuilds the dedicated refused surface.
        # Poll the TEXT, not just visibility, so a transient wrong message
        # never wins the assertion (convention 14).
        def _refusal_text():
            if not driver.is_visible(sentence_id):
                return None
            text = driver.get_text(sentence_id)
            return text if S.onboarding.launch.sign_in_refused in (text or "") else None

        message = wait_until(
            _refusal_text,
            ROUTE_TIMEOUT,
            diagnose=lambda: (
                f"{sentence_id!r} reads {driver.get_text(sentence_id)!r}"
                if driver.is_visible(sentence_id)
                else f"{sentence_id!r} not visible"
            ),
        )
        assert S.onboarding.launch.sign_in_refused in (message or ""), (
            f"the mid-session refusal lands the refused surface's sentence in "
            f"{sentence_id!r}, got {message!r}"
        )
        assert driver.is_visible(LAUNCH_FALLTHROUGH), (
            "the refused surface keeps 'Use a different nest'"
        )
        if not has_notice:
            skip_unbuilt(
                driver,
                surface=SIGN_IN_REFUSED_NOTICE,
                detail="the mid-session escalation lands; the sentence still "
                "paints in error-message, with no Retry (checked above)",
            )
        assert driver.is_visible(LAUNCH_RETRY), "the refused surface offers Retry"
    finally:
        admin("fauna.admin.users.cancel_eviction")

    driver.click(LAUNCH_RETRY)
    reached_authenticated_app(driver, timeout=90)


# ---------------------------------------------------------------------------
# F — "Use a different nest" fallthrough on the retry surface → handle_entry
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("launch_harness", _clients(*WEB_FIT), indirect=True)
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_f_retry_surface_fallthrough_goes_to_handle_entry(launch_harness):
    """From the retry surface, clicking `launch-fallthrough-button` ("Use a
    different nest") seeds the identity and lands the wizard at `handle_entry`
    (onboarding.md row 509 fallthrough). This is the current home of the old
    procedure-doc "case D" intent."""
    dead_url = f"http://127.0.0.1:{_free_closed_port()}"
    driver = launch_harness.launch_and_route(
        secret_hex=secrets.token_hex(32), node_url=dead_url, trust=None
    )
    driver.wait_for(LAUNCH_FALLTHROUGH, timeout=ROUTE_TIMEOUT)
    driver.click(LAUNCH_FALLTHROUGH)
    driver.wait_for(HANDLE_INPUT, timeout=15)
    assert driver.is_visible(HANDLE_INPUT), (
        "fallthrough must land the wizard at handle_entry so the user can pick "
        "a different nest"
    )


# ---------------------------------------------------------------------------
# G — factory reset → the next launch is a fresh install (identity_choice)
# ---------------------------------------------------------------------------
# NATIVE ONLY: web's `reset()` sweeps the whole `fauna_*`/`fauna/*` localStorage
# namespace as the TEST AGENT's cleanup (`web-bridge/agent.js`), not through the
# product path, so a reset-erase assertion greens on the harness and pins nothing;
# `WebCredStore.stored_accounts()` is deliberately unimplemented, and a browser
# `teardown()` discards the whole profile so there is no force-quit analogue.
@pytest.mark.parametrize("launch_harness", _clients(*NATIVE_ONLY), indirect=True)
@pytest.mark.feature("factory-reset")
def test_smoke_g_factory_reset_leaves_no_identity_for_the_next_launch(
    nest_instance, launch_harness
):
    """A factory reset must clear the client's WHOLE credential namespace, so the
    next launch takes the routing table's "nothing stored" row (wizard at
    `identity_choice`) rather than silently challenging the reset identity back
    into its session (onboarding.md § App-launch routing, row 5).

    Clearing wholesale is the point, not a tidiness nicety. `AccountRegistry`
    keeps the active-account pointer in `fauna/index` alongside the per-actor rows;
    a reset that drops only the per-actor rows leaves `active` pinned to the reset actor,
    and `RegistryLaunchPersistence::load_identity()` then serves THAT actor's
    secret to the launch machine while the next sign-in builds its `AuthClient`
    around a different keypair. The nest rejects the pairing at the WS handshake
    (`security.md` § Cross-connection binding) with a 403, the reconnect
    supervisor retries it forever, and every WS-RPC call hangs instead of
    failing — an empty page with a blank `error-message`. windows
    (`registry.ClearAll()`), android (`storage.clear()`) and linux
    (`delete_namespace`) all wipe the namespace (`fauna/index` plus per-actor rows) on reset; this
    pins that.

    No-modes retirement (ratified 2026-07-12): a nest is content-ready from
    first boot now, so the pre-reset launch lands in the authenticated main
    app (the Online row) with no preliminary commit needed.
    """
    admin_secret_hex = bytes(nest_instance["admin"]["signing_key"]).hex()
    driver = launch_harness.launch(
        secret_hex=admin_secret_hex, node_url=nest_instance["url"], trust=nest_instance
    )
    # Reset the INSTANT the session flag flips — deliberately racing the launch
    # machine's in-flight `save_authenticated`, which is still writing the
    # namespace on a worker the client cannot cancel (linux spawns it as a
    # detached `std::thread`). That race is the regression this pins.
    #
    # This used to additionally wait for the authenticated shell (`feed-tab`)
    # before resetting. That wait was not coverage — it was avoidance: reading
    # `AccountRegistry::index()` used to write as a side effect, so an erase landing inside
    # that window left credentials (the identity secret among them) behind, and the settle merely let the
    # writer finish first. Reads are pure now and the wipe runs last, so a writer
    # that races the erase finds no active account and writes nothing
    # (`long-term-store.md` § Cleanup contract). Restoring the settle would
    # re-hide exactly the bug this case exists to catch.
    driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
    )

    # Precondition for the account-store half below: sign-in must have minted
    # the shared fauna-account-store namespace's writer key / principal-bundle
    # slots, or that assertion would pass vacuously (long-term-store.md §
    # Cleanup contract hole 3). The mint is a task spawned OFF the login path
    # (`apps/fauna-linux/src/account_runtime.rs`'s `install` module docs), so
    # a read straight after `authenticated` flips races it — this barrier is
    # unrelated to the deliberate reset-vs-save_authenticated race above (that
    # one is the CREDENTIAL store; this is the separate account-store
    # namespace, and it must be settled BEFORE the reset this test drives
    # next).
    await_account_runtime_assembled(launch_harness.driver)
    account_before = launch_harness.account_store.stored_accounts()
    assert account_before, (
        "precondition: sign-in must have minted the shared fauna-account-store "
        "namespace too — an empty account-store namespace here means the "
        f"post-reset half of this test would assert nothing (client={launch_harness.client})"
    )

    driver.reset()
    assert driver.get_state("session")["authenticated"] is False, (
        "reset must sign the session out"
    )
    # Minus the install device secret: install-scoped, never an identity slot,
    # and deliberately kept by windows' reset, whose e2e arm is sign-out-shaped
    # (`e2e-launch-isolation.md` convention 10). It resolves no account, so the
    # routing asserted below is indifferent to it.
    leftovers = launch_harness.store.stored_accounts() - {INSTALL_DEVICE_SECRET_SLOT}
    assert not leftovers, (
        "a factory reset must empty the credential namespace, but these slots "
        f"survived it: {sorted(leftovers)}. `fauna/index` among them pins the "
        "active account to the reset actor and 403-hangs the next sign-in."
    )

    # The account-store half (hole 3, closed 2026-09-01): a reset that wipes
    # only the app's own namespace leaves this machine's writer key and the
    # reset actor's principal bundle recoverable in the shared namespace.
    account_leftovers = launch_harness.account_store.stored_accounts()
    assert not account_leftovers, (
        "a factory reset must ALSO empty the shared fauna-account-store "
        f"namespace (long-term-store.md § Cleanup contract, hole 3), but these "
        f"slots survived it: {sorted(account_leftovers)}. "
        f"(before={sorted(account_before)})"
    )

    # The behavioral proof: with the namespace empty there is no identity to
    # hydrate, so the launch machine routes to `WizardAt{IdentityChoice}`.
    launch_harness.relaunch()
    driver.wait_for(CREATE_IDENTITY, timeout=30)
    assert driver.is_visible(CREATE_IDENTITY), (
        "after a factory reset the next launch must be a fresh install "
        "(identity_choice), not a silent challenge back into the old session"
    )
    assert driver.get_state("session")["authenticated"] is False


# ---------------------------------------------------------------------------
# A — a submitted pending invite SURVIVES a force-quit (the real save half)
# ---------------------------------------------------------------------------
# NATIVE ONLY: the real submit+persist+rehydrate is already covered on web by
# `tests/web/test_pending_invite_persistence.py` + `..._recheck_404.py`; joining
# here would duplicate it (priority #4), and the case is bound to the native
# file-backed cred store (`headless_credential_store` → `file_backed`), which the
# web localStorage adapter has no analogue for.
@pytest.mark.headless_credential_store
@pytest.mark.parametrize("launch_harness", _clients(*NATIVE_ONLY), indirect=True)
@pytest.mark.feature("join-a-nest")
def test_smoke_a_pending_invite_survives_force_quit(nest_instance, launch_harness):
    """The faithful save+load case (the others inject; this one drives the real
    submit). Import an identity (persists the secret), submit a real invite request
    against the claimed nest (`wizard_submit_invite_request` →
    `handle_wizard_done(InviteSubmitted)` → the shared `AccountRegistry`
    pending-invite slot; NO long-term node_url is written, only on LoggedIn),
    reach PendingReview, then FORCE-QUIT (the driver kills the process group) and
    relaunch. The relaunch must read `(identity, no node_url, pending-invite)` →
    the startup branch `WizardAt{InviteRequest}` with `seed_pending_invite`, so the
    page returns to invite_request with the PendingReview state rehydrated — the
    recheck affordance, which only PendingReview reveals.

    Runs on the FILE-backed credential store (`headless_credential_store` →
    `file_backed`), not the real-keyring `LibsecretCredStore` the rest of this
    module uses for linux: the pending-invite slot lives on the shared
    `AccountRegistry` (`fauna-credential-store`), which honors
    `FAUNA_E2E_CREDENTIAL_DIR` independently of an unlocked session Secret Service
    — proof the lift needs no real keyring, unlike the old bespoke libsecret-only
    wrapper it replaced."""
    from actions import ActionLayer

    secret_hex = bytes(SigningKey.generate()).hex()
    # Fresh wizard (no seeded identity) pointed at the claimed nest.
    driver = launch_harness.launch(node_url=nest_instance["url"], trust=nest_instance)
    app = ActionLayer(driver)
    # Import the identity (identity_import.rs persists the secret to the store
    # immediately, so the relaunch skips the identity stage).
    app.onboarding.navigate_to_status()
    app.onboarding.import_key(secret_hex)
    driver.wait_for(HANDLE_INPUT, timeout=15)

    # Jump to invite_request for this claimed nest (a test nest isn't
    # DNS-discoverable) and drive the REAL submit over the anonymous WS — same
    # path as test_invite_request_submit_roundtrip.py.
    driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_instance["url"], "smokejoiner"]),
    )
    driver.wait_for(INVITE_SUBMIT, timeout=20)
    driver.click(INVITE_SUBMIT)

    # PendingReview is the only state that reveals the recheck affordance.
    deadline = time.monotonic() + 20.0
    while time.monotonic() < deadline and not driver.is_visible(INVITE_RECHECK):
        time.sleep(0.5)
    assert driver.is_visible(INVITE_RECHECK), (
        "invite submit did not reach PendingReview before the force-quit "
        f"(the real invite_request.submit over WS failed). error: {app.error_text()!r}"
    )

    # The pending-invite slot is persisted on the `wizard_submit_invite_request()`
    # RETURN VALUE, the moment the snapshot transitions to `PendingReview` — the
    # only write moment (onboarding.md § Long-term store contract; the former
    # continue-exit's duplicate save at Continue is retired, 2026-08-12: Continue
    # is now the out-of-band code's redeem and nothing else, disabled throughout
    # PendingReview). The `INVITE_RECHECK` wait above already synchronized on
    # that transition, so the slot is already in the store — no click owed here.

    # Force-quit + relaunch: the pending invite must rehydrate to invite_request.
    slots_before = launch_harness.store.stored_accounts()
    launch_harness.relaunch()
    # e2e rule 6 — a bare `wait_for` here times out saying only "element absent",
    # which cannot distinguish the two halves that can break: the wizard-exit
    # never PERSISTED the pending-invite slot, or it persisted and the relaunch
    # MIS-ROUTED. Poll, then name both the surviving slots and the surface that
    # actually came up, so the failure classifies itself in one run.
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline and not driver.is_visible(INVITE_RECHECK):
        time.sleep(0.3)
    if not driver.is_visible(INVITE_RECHECK):
        landed = [
            el for el in (HANDLE_INPUT, CREATE_IDENTITY, INVITE_SUBMIT, "feed-tab")
            if driver.is_visible(el)
        ]
        raise AssertionError(
            "the relaunch did not rehydrate invite_request at PendingReview.\n"
            f"  credential slots BEFORE the force-quit: {sorted(slots_before)}\n"
            f"  credential slots AFTER  the relaunch:   "
            f"{sorted(launch_harness.store.stored_accounts())}\n"
            f"  surface that came up instead: {landed or '<none of the known anchors>'}\n"
            "A missing pending-invite slot in the BEFORE list means the wizard exit "
            "never persisted it; a present slot with a different surface means the "
            "launch machine mis-routed a stored pending invite."
        )
    # The wizard animates `set_visible_child_name` (SlideLeftRight), so the
    # prior stack child can stay mapped for a few hundred ms after the target
    # page maps. Settle before the negative assertion, on the SAME predicate the
    # assertion uses (`is_absent`: is_visible == is_mapped on linux, tree
    # membership on windows) so the loop cannot exit on an offscreen-but-present
    # page the assertion then reads as still there.
    deadline = time.monotonic() + 6.0
    while time.monotonic() < deadline and not driver.is_absent(HANDLE_INPUT):
        time.sleep(0.3)
    assert driver.is_visible(INVITE_RECHECK), (
        "a submitted pending invite did not survive force-quit + relaunch: the "
        "app should have read the persisted pending-invite slot and rehydrated "
        "invite_request at PendingReview (recheck affordance)"
    )
    assert driver.is_absent(HANDLE_INPUT), (
        "relaunch dropped to the wizard's handle/identity stage — the "
        "pending-invite slot did not survive the force-quit"
    )


# ---------------------------------------------------------------------------
# H — RETIRED (no-modes, ratified 2026-07-12)
# ---------------------------------------------------------------------------
# `test_smoke_h_deferred_mode_choice_relaunches_to_encryption_mode_choice`
# pinned the "admin-claimed, mode-unresolved" launch-routing row: claim an
# unclaimed nest, defer the storage-mode choice (`AwaitingEncryptionMode`),
# force-quit, and assert the relaunch re-seeds `encryption_mode_choice`
# instead of falling through to the authenticated main app.
#
# That row is explicitly retired, not just its UI: `docs/goal/behavior/
# onboarding.md` § App-launch routing, "Admin-claimed, mode-unresolved —
# RETIRED as a native wizard route" — target state has no unresolved-mode
# window (the `storage_mode_pending` flag a no-modes nest once reported as a
# constant `false` left the wire 2026-09-24; a successful verify lands
# Online). The replacement terminal step, `nat_mode_choice`,
# is confirm-only with a working seeded default and its own defer button
# EXITS TO LOGGEDIN (never strands the admin) — there is no more
# claimed-but-blocked state for a launch to route back into. The scenario
# this test proved can no longer occur, so it is deleted rather than
# reworked (mirrors `test_encryption_mode_choice.py`'s full retirement) —
# not a rework target, since there is no successor state to assert.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# I — deferred DNS → relaunch returns to the "Almost ready" surface
# ---------------------------------------------------------------------------
# NATIVE ONLY: already covered on web by `tests/web/test_awaiting_dns_persistence.py`
# (both relaunch-hydration AND the real `dns_post_instructions` exit); joining
# here would duplicate it (priority #4).
AWAITING_DNS_RECORDS = "awaiting-dns-records"
AWAITING_DNS_STATUS = "awaiting-dns-status"
AWAITING_DNS_RECHECK = "awaiting-dns-recheck-button"

#: The nest being provisioned. Deliberately a name that cannot resolve: that IS
#: the deferred-DNS state — the records aren't at the registrar yet, so the box is
#: unreachable and `recheck_manual_dns()` keeps returning Pending. Pointing this
#: at the live test nest instead would let the first poll claim it and route the
#: surface away mid-assertion.
PROVISIONING_NEST = "http://nest.smoke-awaiting-dns.invalid:8080"
# The record the registrar still needs, in the "TYPE NAME VALUE" shape
# `go_to_dns_post_instructions_with_records` parses.
PROVISIONED_RECORD = "A nest.smoke-awaiting-dns.invalid 203.0.113.7"


@pytest.mark.parametrize("launch_harness", _clients(*NATIVE_ONLY), indirect=True)
@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_smoke_i_deferred_dns_relaunches_to_the_almost_ready_surface(launch_harness):
    """The awaiting-manual-dns launch-routing row (onboarding.md § App-launch
    routing) + the "Almost ready" surface (§ "Almost ready" surface).

    A user provisions their own nest, picks "Set up later" for DNS, and quits
    before the records propagate. The next launch must NOT drop them at
    handle_entry (losing a half-provisioned box) and must NOT silent-challenge a
    nest that cannot answer — it must read the awaiting-manual-dns slot, seed the
    wizard, and re-render "Almost ready" with the records still to be added.

    The slot is written by the client's OWN wizard-exit handling, and the exit
    here is REAL: the admin clicks Continue on `dns_post_instructions`, the
    production `continue_from_dns_post_instructions()` sets
    `wizard_outcome() = AwaitingManualDns`, `handle_wizard_done` persists the
    slot, and the shared `LaunchMachine` — reading that same slot through
    `RegistryLaunchPersistence` — routes the relaunch to
    `WizardAt{AwaitingManualDns}` on its own, with no pre-machine branch.

    What is *arranged* rather than clicked is only the state the exit starts
    FROM — the provisioned nest_url, the captured records, and the provisioning
    result carrying the claim code. Reaching that state for real means a live
    cloud provisioning run, which is a different test's subject; arranging
    preconditions is explicitly permitted, driving the behavior under test
    through an API is not. So the ARRANGEMENT is seeded and the EXIT is clicked.
    (Until tui grew the §§ 4–7 pages this whole flow had to be seeded via
    `seed_awaiting_manual_dns`, which tested the client's persistence of an
    outcome the client never actually produced.)

    The nest_url points at a `.invalid` host on purpose: that IS the deferred-DNS
    state — the records are not at the registrar yet, so the box is unreachable.
    Point it at a live nest and the first `recheck_manual_dns()` poll claims it
    and routes the surface away mid-assertion.
    """
    from actions import ActionLayer

    # (windows was `skip_unbuilt`-gated here until 2026-08-09: its agent published
    # `session.authenticated` as "an identity key is loaded", so this case's final
    # assertion failed while every surface assertion above it passed. The agent now
    # derives the flag from the mounted page — e2e-conventions.md § convention 11 →
    # *what `session.authenticated` means* — so the gate is gone, not widened.)

    secret_hex = bytes(SigningKey.generate()).hex()
    # Fresh wizard (no seeded identity) pointed at the provisioning (dead) nest.
    driver = launch_harness.launch(node_url=PROVISIONING_NEST, trust=None)
    app = ActionLayer(driver)
    # Import an identity: the secret lands in the store, so the relaunch has
    # an identity to pair the slot with (the row is identity + slot).
    app.onboarding.navigate_to_status()
    app.onboarding.import_key(secret_hex)
    driver.wait_for(HANDLE_INPUT, timeout=15)

    # ── Arrange: the post-provisioning state the deferred-DNS exit starts
    # from. `continue_from_dns_post_instructions()` reads the claim code off
    # `provisioning_snapshot().result` but the nest_url off machine state, so
    # both have to be in place before the click.
    driver.call_machine_method("set_nest_url", json.dumps(PROVISIONING_NEST))
    app.onboarding.go_to_dns_post_instructions_with_records([PROVISIONED_RECORD])

    # ── Act: the real exit. This click is the entry point to everything the
    # test asserts — the outcome, the slot write, and the relaunch routing.
    app.click("dns-post-instructions-continue-button")

    # Same-session: the surface renders off `wizard_outcome()`.
    driver.wait_for(AWAITING_DNS_RECORDS, timeout=15)
    assert "203.0.113.7" in driver.get_text(AWAITING_DNS_RECORDS), (
        "the 'Almost ready' surface must show the records the user still has "
        f"to add. error: {app.error_text()!r}"
    )
    assert driver.is_visible(AWAITING_DNS_STATUS)
    time.sleep(2)  # let handle_wizard_done's slot write land before the kill

    launch_harness.relaunch()

    # The whole point: no seeding this time. The launch machine read the slot.
    driver.wait_for(AWAITING_DNS_RECORDS, timeout=30)
    assert driver.is_visible(AWAITING_DNS_RECHECK), (
        "a deferred-DNS nest did not survive force-quit + relaunch: the launch "
        "machine should have read the awaiting-manual-dns slot and rehydrated "
        "the 'Almost ready' surface"
    )
    assert "203.0.113.7" in driver.get_text(AWAITING_DNS_RECORDS), (
        "the surface rehydrated but the DNS records did not survive the slot "
        "— the user would have nothing to add at their registrar"
    )
    assert driver.is_absent(HANDLE_INPUT), (
        "relaunch dropped to the wizard's handle stage — the half-provisioned "
        "nest was lost"
    )
    # Deadline-poll for the session key to exist at all (convention 14) rather
    # than a raw single read: dropping the harness's now-unconditional
    # --nest-url made this relaunch faster (no dead-host connection
    # attempt), which was tight enough to occasionally race the TestAgent's
    # first post-relaunch state push and read `None` before it ever landed.
    state = driver.wait_for_state(lambda s: "session" in s, timeout=10)
    assert state["session"]["authenticated"] is False, (
        "the nest is not claimed yet — the relaunch must not enter the main app"
    )


# ---------------------------------------------------------------------------
# J — `--autostart`: launch ROUTING decides whether a window opens at all
# ---------------------------------------------------------------------------
# AUTOSTART_APPS only — see that constant for why the set is one app today.
@pytest.mark.launch_args("--autostart")
@pytest.mark.parametrize("launch_harness", _clients(*AUTOSTART_APPS), indirect=True)
@pytest.mark.feature("general-settings")
def test_smoke_j_autostart_stays_hidden_only_when_it_lands_in_the_main_app(
    nest_instance, launch_harness
):
    """The sign-in auto-start launch is tray-resident **conditionally**, and the
    condition is where launch routing lands (`apps/windows.md` § App Lifecycle →
    *Auto-start at sign-in*):

    * routed to the main app (`identity + nest_url` → silent challenge → Online;
      `onboarding.md` § App-launch routing row 1) ⇒ **no window** — everything
      residency needs is code-driven, and a window over a freshly-restored desktop
      every morning is the whole thing shape A exists to avoid;
    * routed to onboarding (nothing stored; row 5) ⇒ **the window shows** — "a
      signed-out auto-start must be loud, not a silently dead agent".

    Shipped 2026-07-16 with **no headless test**; this is it.

    **Both arms in one case, deliberately.** The hidden arm is a NEGATIVE assert,
    which convention 14 forbids anchoring to a settle-sleep — so it anchors to the
    app's PUBLISHED activation decision (present only after the gate ran) and is
    corroborated at the OS level. But a negative that only ever runs against a
    hidden arm is also satisfied by an app that can no longer open a window at
    all, by a `--autostart` flag silently dropped somewhere between the harness and
    `GetCommandLineArgs`, or by a bridge route that always answers "no windows".
    The shown arm is the control that kills all three: same flag, same binary, same
    OS query, opposite answer.

    **Two independent witnesses per arm.** `state["launch"]["window_shown"]` is the
    app's self-report — it proves the code took the branch it meant to. The OS's
    top-level-window list proves the branch had the effect it claims. A self-report
    alone would green on a gate that decides correctly and then activates anyway.
    """
    # ── Arm 1: a registered identity on a live nest → Online → stay hidden.
    # The nest's own claimed-admin identity is registered by construction, so the
    # silent challenge really succeeds (same actor smoke B uses).
    driver = launch_harness.launch(
        secret_hex=bytes(nest_instance["admin"]["signing_key"]).hex(),
        node_url=nest_instance["url"],
        trust=nest_instance,
    )
    # Deliberately state-only until the decision is read: with no window there is
    # nothing for FlaUI to attach to, and a UI query here would fail as a bridge
    # timeout — i.e. as infrastructure, not as the assertion it stands in for.
    reached_authenticated_app(driver, timeout=90)
    hidden = _await_launch_decision(driver)
    assert hidden["autostart"] is True, (
        "the app did not see --autostart, so this arm proves nothing about tray "
        "residency — the flag was dropped between the harness's launch config and "
        f"the app's command line. published launch state: {hidden!r}"
    )
    assert hidden["window_shown"] is False, (
        "an --autostart launch that routed into the main app activated its window "
        "anyway: a user who signed in would get the app opening over their fresh "
        "desktop, which is exactly what tray residency exists to avoid "
        f"(published launch state: {hidden!r})"
    )
    on_screen = _visible_windows(driver)
    assert not on_screen, (
        "the app reported it skipped Activate(), but the OS says these windows are "
        f"on screen: {on_screen!r}. The decision is right and its effect is wrong "
        "— something after the gate surfaces the window (tray init, the "
        "single-instance listener, or a page forcing activation)."
    )

    # ── Arm 2 (the control): same flag, same binary, nothing stored → onboarding
    # → the window MUST show. Clearing the credential namespace is what moves the
    # launch from routing row 1 to row 5; `relaunch()` keeps the pinned data dir,
    # so this is the same device restarting, not a new one.
    launch_harness.store.clear()
    launch_harness.relaunch()
    shown = _await_launch_decision(driver)
    assert shown["autostart"] is True, (
        f"the relaunch lost --autostart; the control arm is void. state: {shown!r}"
    )
    assert shown["window_shown"] is True, (
        "an --autostart launch with nothing stored routed to onboarding and stayed "
        "HIDDEN: the user is signed out with no window and no way to notice — a "
        "silently dead agent, which the goal doc rules out explicitly "
        f"(published launch state: {shown!r})"
    )
    # The window is the *visible* half of "loud", so assert the surface too, and
    # let it prove the wizard really came up rather than an empty frame.
    driver.wait_for(CREATE_IDENTITY, timeout=ROUTE_TIMEOUT)
    assert driver.is_visible(CREATE_IDENTITY), (
        "a signed-out auto-start showed its window but not the wizard"
    )
    on_screen = _visible_windows(driver)
    assert on_screen, (
        "the app reported it activated its window, but the OS lists none on screen "
        "— so the hidden arm above proves nothing: this query cannot tell a hidden "
        "window from a shown one."
    )


# ---------------------------------------------------------------------------
# K — a REAL onboarding completion reaches the authenticated main app
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("launch_harness", _clients(*NATIVE_ONLY), indirect=True)
@pytest.mark.headless_credential_store
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_k_real_onboarding_completion_reaches_the_main_app(
    smoke_handled_nest, smoke_handled_actor, launch_harness
):
    """The routing table's row 1 (`onboarding.md` § App-launch routing: identity
    present + nest_url present → main app), reached the way a real user reaches it
    — by FINISHING THE WIZARD, with the handle check run against a live nest.

    **Why this case exists at all.** Every other client-side proof of "onboarding
    lands you in the app" either seeds a stored identity and launches (cases B/E
    above), logs in through the agent's `set_state`, or INJECTS the handle-check
    outcome via `set_handle_check_snapshot`. None of those runs the wizard's own
    terminal hand-off, so none of them can fail when that hand-off breaks — and on
    windows nothing proved it at all until this case (opened when row 62 made `session.authenticated` honest and a probe found the
    app on none of feed / handle_entry / identity_choice 60 s after Continue).

    **Native-only, structurally.** The typed `<localpart>@127.0.0.1:<port>` handle
    resolves to `https://127.0.0.1:<port>` and the check dials that nest DIRECTLY;
    a browser cannot (it reaches nests through the CORS `spa_url` proxy, which is
    a different origin than the handle resolves to). Same class as case A/G, not a
    web gap.

    The arrival barrier is `reached_authenticated_app` — on native, `session.
    authenticated` means "the main app is mounted" since row 62, which is exactly
    the fact under test. Do NOT weaken it to a `feed-tab` `is_visible`: the apps
    land on different default views, which is the drift that barrier already owns.
    """
    from actions import ActionLayer

    secret_hex = bytes(smoke_handled_actor["signing_key"]).hex()
    typed_handle = f"{smoke_handled_actor['handle']}@{smoke_handled_nest['authority']}"

    # Fresh client, NOTHING stored → the routing table's last row (wizard at
    # identity_choice). The harness's `node_url` only points the launch machine at
    # a live nest; it stores no identity, so the wizard really does run.
    driver = launch_harness.launch(node_url=smoke_handled_nest["url"], trust=smoke_handled_nest)
    app = ActionLayer(driver)

    app.onboarding.navigate_to_status()
    app.onboarding.import_key(secret_hex)  # lands on handle_entry
    driver.wait_for(HANDLE_INPUT, timeout=ROUTE_TIMEOUT)
    app.onboarding.fill_handle(typed_handle)
    # The REAL check — no `set_handle_check_snapshot`. The actor is already
    # registered on this nest, so it resolves AlreadyOnNest{handle_matches: true}
    # from the wire, which is what makes the wizard produce Done → LoggedIn.
    app.onboarding.run_handle_check(timeout=45)
    # RED-VERIFIED 2026-08-10, and the verify is the only reason this case counts
    # as coverage rather than a 12-minute green: commenting out this one line
    # reds it in 2m13s with `visible={'handle-input': True, 'feed-tab': False}` —
    # i.e. the failure message correctly says "the wizard never completed"
    # rather than timing out anonymously. Arrival really is caused by the
    # wizard's terminal Continue, not by anything the harness does around it.
    app.onboarding.submit_handle()

    # The welcome-back path may surface a terminal wizard step before the app
    # mounts (the NAT-mode confirm per onboarding.md § 3b-bis, or a launch retry).
    # Clicking through whatever appears is the user's own next action, not a
    # settle-sleep: the loop's exit condition is arrival, and the budget is only
    # ever spent by a genuine failure. Prior art:
    # `test_mail_auto_enable_first_setup.py::_drive_first_setup_to_logged_in`.
    deadline = time.monotonic() + 120.0
    arrived = False
    while time.monotonic() < deadline:
        try:
            reached_authenticated_app(driver, timeout=2)
            arrived = True
            break
        except Exception:
            pass
        for btn in ("nat-mode-confirm-button", LAUNCH_RETRY):
            with contextlib.suppress(Exception):
                if driver.is_visible(btn):
                    driver.click(btn)

    if not arrived:
        # Self-diagnosing, because the two candidate causes need different fixes
        # and a bare timeout cannot tell them apart (the whole first move):
        # still ON a wizard page ⇒ the wizard never completed; on NO known surface
        # ⇒ it completed and the hand-off dropped the user nowhere.
        surfaces = {}
        for eid in (HANDLE_INPUT, CREATE_IDENTITY, "feed-tab", LAUNCH_TRANSIENT_ERROR):
            with contextlib.suppress(Exception):
                surfaces[eid] = driver.is_visible(eid)
        with contextlib.suppress(Exception):
            surfaces["session"] = driver.get_state().get("session")
        pytest.fail(
            "a REAL onboarding completion never reached the authenticated main "
            "app. The wizard ran its own handle check against a live nest and "
            "Continue was submitted, so this is the post-wizard hand-off "
            f"(onboarding.md § App-launch routing, row 1). visible: {surfaces!r}; "
            f"app error: {app.error_text()!r}"
        )

    assert driver.is_absent(HANDLE_INPUT), (
        "the app reports an authenticated session while the handle-entry page is "
        "still up — the arrival flag and the rendered surface disagree, which is "
        "the vacuous-green shape row 62 closed; do not trust the flag until this "
        "is explained."
    )


# ---------------------------------------------------------------------------
# L — a wrong client clock still signs in on launch (login.md § Goal)
# ---------------------------------------------------------------------------
#: Six hours, and BEHIND. Hours, not seconds: far outside the handshake's ±30 s
#: window, so this is a skew the OTHER pre-identity ceremony refuses outright
#: (the control below proves it against the same nest). Behind, not ahead: a
#: clock nobody has set yet — the freshly-booted, NTP-not-yet-run device
#: `login.md` § Silent Challenge names — runs behind. The AHEAD direction is
#: case M's: it is the direction that used to fire the bearer's TTL refresh at
#: once and get it refused on drift, so it witnesses the refresh, not the launch.
WRONG_CLOCK_OFFSET_SECS = -6 * 60 * 60


@pytest.mark.parametrize(
    "launch_harness",
    _clients("linux", "tui", "web", "macos", "ios", "android", "windows"),
    indirect=True,
)
@pytest.mark.launch_env(FAUNA_E2E_CLOCK_OFFSET_SECS=WRONG_CLOCK_OFFSET_SECS)
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_l_wrong_clock_still_signs_in(nest_instance, launch_harness, live_node_url):
    """A device whose clock is wrong still signs you in when the app opens
    (`docs/features/connect-and-sign-in.md` outcome 9; `login.md` § Goal — the
    silent challenge signs `actor_id ‖ nonce`, no timestamp, so it is immune to
    client clock skew).

    Case B's launch with one difference: the app's clock is six hours wrong. The
    wrong clock is the compile-gated launch-clock seam
    (`fauna_launch_machine::launch_clock`, seeded from
    `FAUNA_E2E_CLOCK_OFFSET_SECS` before `LaunchMachine::start()`; convention 15
    — absent from a release build), never the box's clock: the machines are
    shared.

    **The green is not vacuous, and the test says why.** Launch takes the silent
    challenge unconditionally, selected by no clock read, so an offset injected
    into a clock the ceremony never consults would pass this test while proving
    nothing. Two controls close that: (a) below, the app's OWN clock is read
    back and must be the wrong one, so the seed reached the process that signed
    in; (b) the sibling test — the same skew is refused by the handshake, the
    ceremony that DOES carry a timestamp, on the same nest, so the skew is
    material. (No in-app ceremony signs a timestamp any more: every bearer the
    launch machine mints, refresh included, is the silent challenge — `login.md`
    § When to use which; case M below is the refresh's witness.) The seam is
    shared Rust and reaches every native app through the same crate; web has no
    environment, so its harness writes the same name into localStorage, which
    the launch chunk's test flavor reads before `start()`. What trickles down
    per app is the `clock` state key its agent publishes for control (a)
    (`fauna_e2e_agent::CLOCK_KEY`): all seven apps publish it today.
    """
    admin_secret_hex = bytes(nest_instance["admin"]["signing_key"]).hex()
    driver = launch_harness.launch_and_route(
        secret_hex=admin_secret_hex, node_url=live_node_url, trust=nest_instance
    )
    reached_authenticated_app(driver, timeout=90)
    assert driver.is_absent(HANDLE_INPUT), "should not be in the wizard"

    # Control (a): the process that just signed in is running on the wrong clock.
    clock = driver.get_state("clock") or {}
    assert clock.get("offset_secs") == WRONG_CLOCK_OFFSET_SECS, (
        "the launch-clock seed never reached the app — a green silent challenge "
        f"on the REAL clock witnesses nothing. clock state: {clock!r}"
    )
    # And the offset is applied, not merely recorded: the app's `now` reads hours
    # behind the harness's. A value comparison with a generous slack, never a
    # wait (convention 14) — six hours cannot be confused with box latency.
    harness_now = time.time()
    assert clock.get("now_secs", harness_now) < harness_now + WRONG_CLOCK_OFFSET_SECS + 600, (
        f"the app's clock is not ~6 h behind: app now={clock.get('now_secs')!r}, "
        f"harness now={harness_now:.0f}"
    )


def test_smoke_l_control_the_handshake_refuses_the_same_skew(nest_instance):
    """Case L's red control (convention 5): the SAME six-hour skew, signed into the
    other pre-identity ceremony — `fauna.auth.handshake`, which carries a client
    timestamp the nest holds to ±30 s (`login.md` § Direct Auth) — is refused
    with `fauna.auth.timestamp_drift` by the nest case L signs into. Proves the
    skew case L survives is one that matters, so the immunity is a claim about
    WHICH ceremony launch takes, not about a nest that tolerates anything. The
    unskewed twin mints first, so the refusal is the skew's and not a broken
    request's. No app: the nest is the subject, observed from outside.
    """
    import secrets

    from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient
    from common.nest_identity import read_nest_identity
    from common.sig_domain import handshake_signed_message

    sk = SigningKey(bytes(nest_instance["admin"]["signing_key"]))
    actor_id = bytes(sk.verify_key)

    def handshake(timestamp_ms: int) -> dict:
        with WsRpcAnonClient(nest_instance["url"]) as anon:
            nest_id = read_nest_identity(anon)
            client_nonce = secrets.token_bytes(32)
            payload = {
                "actor_id": actor_id.hex(),
                "timestamp": timestamp_ms,
                "signature": sk.sign(
                    handshake_signed_message(actor_id, timestamp_ms, nest_id, client_nonce)
                ).signature.hex(),
                "client_nonce": client_nonce,
                "nest_id": nest_id.hex(),
            }
            return anon.call("fauna.auth.handshake", payload)

    assert handshake(int(time.time() * 1000))["token"], "a right-clock handshake must mint"
    with pytest.raises(RpcCallError) as refused:
        handshake(int(time.time() * 1000) + WRONG_CLOCK_OFFSET_SECS * 1000)
    assert refused.value.code == "fauna.auth.timestamp_drift", refused.value


# ---------------------------------------------------------------------------
# M — a wrong client clock STAYS signed in: the bearer refresh survives it
#     (login.md § When to use which, § Token lifetime on the client's clock)
# ---------------------------------------------------------------------------
#: Six hours AHEAD — the direction case L deliberately avoids. Before the
#: 2026-09-21 ruling this launch signed in and was bounced to non-transient
#: Offline at once: the TTL loop computed `expires_at − buffer − now_client`
#: against the nest's absolute deadline, saturated to zero, refreshed
#: immediately over the handshake, and the nest refused the six-hours-ahead
#: timestamp on drift. Behind (case L) never fires a refresh inside a test, which
#: is exactly why the ahead direction owes its own witness.
WRONG_CLOCK_AHEAD_OFFSET_SECS = 6 * 60 * 60
#: The nest's bearer TTL and the client's pre-expiry buffer, as the bounds the
#: app's OWN schedule must fall inside (`auth_core::TOKEN_TTL_SECS`,
#: `fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS`). Literal on purpose: the
#: test asserts the product's contract, not a value read back from the product.
TOKEN_TTL_SECS = 3600
BEARER_REFRESH_BUFFER_SECS = 60


def _own_session_ids_on_nest(nest_instance, harness_ids: set[str]) -> set[str]:
    """The admin actor's live session ids as the NEST lists them
    (`fauna.sessions.list`, `login.md` § Sessions), minus every bearer the
    HARNESS minted to look — convention 5's outside observer of how many bearers
    the app minted.

    `harness_ids` accumulates across calls, and must: each snapshot opens its own
    client, which mints its own session, and that session is still listed when
    the NEXT snapshot is taken. Subtracting only the current client's id made the
    first snapshot's bearer read as a session the app had gained.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = SigningKey(bytes(nest_instance["admin"]["signing_key"]))
    with WsRpcAdminClient(
        nest_instance["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)
    ) as client:
        harness_ids.add(client.own_token_id)
        listed = {s["token_id"] for s in client.call("fauna.sessions.list", {})["sessions"]}
    return listed - harness_ids


def _launch_token(driver) -> dict:
    token = driver.get_state("launch_token")
    assert isinstance(token, dict), (
        f"the app publishes no `launch_token` state — an app under test that does not "
        f"publish the key, not a schedule that failed. got {token!r}"
    )
    return token


@pytest.mark.parametrize(
    "launch_harness", _clients("tui", "linux", "web", "macos", "ios"), indirect=True
)
@pytest.mark.launch_env(FAUNA_E2E_CLOCK_OFFSET_SECS=WRONG_CLOCK_AHEAD_OFFSET_SECS)
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_m_wrong_clock_ahead_stays_signed_in(nest_instance, launch_harness, live_node_url):
    """A device whose clock is wrong STAYS signed in: the app renews its session
    on its own clock's schedule, and the renewal itself succeeds
    (`docs/features/connect-and-sign-in.md` outcome 10; `login.md` § When to
    use which — every refresh is the silent challenge — and § Token lifetime on
    the client's clock — deadlines are anchored to the client's own clock).

    Case L's launch with the clock six hours AHEAD, then the two legs of the
    ruling, each with its own observable:

    1. **Scheduling.** Right after launch the app's `launch_token` state shows a
       deadline a full TTL away *on its own clock*: `expires_in_secs` in
       `(buffer, TTL]`. The pre-ruling shape — nest-absolute `expires_at` read on
       a clock six hours ahead — computes to about `-6 h`, and a refresh loop
       scheduling on that re-mints in a hot loop after every successful refresh.
       Latency-independent (convention 14): a value read, never a wait, and six
       hours cannot be confused with box latency.
    2. **Ceremony.** The refresh the loop would fire in ~59 minutes is forced now
       (`launch_refresh_token` — the seat's production refresh of the bearer it
       holds, awaited by the agent so the ack lands after the outcome). It must
       SUCCEED on this skewed clock: the app stays authenticated,
       `session_generation` is unchanged (no teardown), the launch error surface
       is not up, the app records exactly one new own session, and that session
       is among the ones the nest gained — observed from outside over
       `fauna.sessions.list` (convention 5). Before the ruling the refresh was the
       ±30 s handshake and this step bounced the app to non-transient Offline.

    Both mint counts also bound the cadence: a launch set that does not grow
    across a barrier, and exactly one more session after the forced refresh, is
    a loop that slept, not one that spun. (How many sessions login mints is the
    seat's own — one on tui, more where login provisions or re-checks.)

    **Whose bearer, and why the witness is not vacuous on any seat.** The
    `launch_token` key and the `launch_refresh_token` command name the bearer the
    app actually holds and refreshes (`fauna_e2e_agent::LAUNCH_TOKEN_KEY`): the
    launch machine's on tui and linux, `getAuthToken`'s cache on web, and
    `FfiNestClient`'s `WsChallengeBearer` on the UniFFI apps. Every one of them
    anchors and compares on the one client clock (`fauna_protocol::client_clock`;
    web reads the launch chunk's copy of it), whose offset the `clock` key proves
    seeded — so `expires_in_secs` a TTL away is computed against that skewed
    `now`, and an anchor on the REAL clock would read hours negative.
    macos and ios publish it from the shared FaunaKit (`AppStateObservables` +
    `LaunchRefreshTokenTestCommand`); windows and android join as their shells
    publish the key (their FFI half is built: `launch_token_json_for_test` /
    `refresh_held_bearer_for_test`).
    """
    admin_secret_hex = bytes(nest_instance["admin"]["signing_key"]).hex()
    driver = launch_harness.launch_and_route(
        secret_hex=admin_secret_hex, node_url=live_node_url, trust=nest_instance
    )
    reached_authenticated_app(driver, timeout=90)
    assert driver.is_absent(HANDLE_INPUT), "should not be in the wizard"

    # Control: the process that signed in is running six hours AHEAD.
    clock = driver.get_state("clock") or {}
    assert clock.get("offset_secs") == WRONG_CLOCK_AHEAD_OFFSET_SECS, (
        f"the launch-clock seed never reached the app; clock state: {clock!r}"
    )
    harness_now = time.time()
    assert clock.get("now_secs", 0) > harness_now + WRONG_CLOCK_AHEAD_OFFSET_SECS - 600, (
        f"the app's clock is not ~6 h ahead: app now={clock.get('now_secs')!r}, "
        f"harness now={harness_now:.0f}"
    )

    # Leg 1 — the schedule is on the app's own clock, a full TTL away.
    token = _launch_token(driver)
    expires_in = token.get("expires_in_secs")
    assert expires_in is not None and BEARER_REFRESH_BUFFER_SECS < expires_in <= TOKEN_TTL_SECS, (
        "the bearer's deadline is not a TTL away on the app's own clock: "
        f"expires_in_secs={expires_in!r}. About -21600 is the pre-ruling shape — the "
        "nest's absolute expires_at read on a clock six hours ahead — whose refresh "
        "loop re-mints in a hot loop (login.md § Token lifetime on the client's clock)."
    )
    launch_ids = set(token.get("own_session_ids") or [])
    assert launch_ids, f"no session recorded at launch: {token!r}"
    # The cadence bound, seat-agnostic: a loop that spins keeps minting, so the
    # set must not grow across a barrier. NOT "exactly one": how many sessions
    # login mints is per-seat product behaviour, not the clock's — linux's
    # sync-agent capability provisioning mints a second through the launch
    # machine, web's login `silentSignIn` beside the launch's primed bearer.
    driver.barrier()
    settled_ids = set(_launch_token(driver).get("own_session_ids") or [])
    assert settled_ids == launch_ids, (
        f"the own-session set kept growing after launch ({launch_ids!r} → "
        f"{settled_ids!r}) — the hot re-mint loop"
    )
    harness_ids: set[str] = set()
    on_nest_before = _own_session_ids_on_nest(nest_instance, harness_ids)
    assert launch_ids <= on_nest_before, (
        f"the app's own session {launch_ids!r} is not among the nest's {on_nest_before!r}"
    )
    generation_before = driver.get_state("session_generation")

    # Leg 2 — the refresh itself, on this clock, succeeds.
    driver.call_command("launch_refresh_token", {}, timeout=60)

    # Still in the authenticated app — each seat's own "I'm in" signal (web's
    # `session.authenticated` is not readable; the helper owns why).
    reached_authenticated_app(driver, timeout=30)
    assert driver.get_state("session_generation") == generation_before, (
        "the refresh tore the authenticated session down (session_generation moved) — "
        "the non-transient Offline landing a refused refresh produces"
    )
    assert driver.is_absent(LAUNCH_TRANSIENT_ERROR), (
        "the launch error surface is up after the refresh"
    )
    token_after = _launch_token(driver)
    expires_in_after = token_after.get("expires_in_secs")
    assert (
        expires_in_after is not None
        and BEARER_REFRESH_BUFFER_SECS < expires_in_after <= TOKEN_TTL_SECS
    ), f"the refreshed bearer's deadline is not a TTL away on the app's own clock: {expires_in_after!r}"
    ids_after = set(token_after.get("own_session_ids") or [])
    assert launch_ids < ids_after and len(ids_after - launch_ids) == 1, (
        f"expected the launch sessions plus exactly one refresh session, got {ids_after!r} "
        f"(launch {launch_ids!r})"
    )
    on_nest_after = _own_session_ids_on_nest(nest_instance, harness_ids)
    # The refresh's session exists nest-side. A superset, not equality: other
    # minters in the same app process (the sync agent's own grant, a login
    # re-check) may land a session of their own in this window — none of them
    # the bearer under test, which `own_session_ids` above already pins.
    assert ids_after - launch_ids <= on_nest_after - on_nest_before, (
        "the app's refresh session never reached the nest: nest gained "
        f"{on_nest_after - on_nest_before!r}, app recorded {ids_after - launch_ids!r}"
    )


# ---------------------------------------------------------------------------
# N — a SUSPENDED user's way back into the wizard ends on an honest terminal
#     sentence, never a "try again" (login.md § Errors, onboarding.md § 3)
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("launch_harness", _clients(*NATIVE_ONLY), indirect=True)
@pytest.mark.headless_credential_store
@pytest.mark.feature("connect-and-sign-in")
def test_smoke_n_suspended_user_fallthrough_invite_submit_is_terminal(
    smoke_handled_nest, launch_harness
):
    """The one route by which a suspended user still reaches the wizard
    (`login.md` § Errors): the refused launch's "Use a different nest" →
    `handle_entry` → the SAME handle, whose check rides the opaque
    challenge/verify and so classifies them unregistered → `invite_request` →
    submit. The nest refuses that submit as already registered
    (`fauna.account.actor_exists`), which is permanent until the admin restores,
    so `invite-request-status` must carry the dedicated
    `onboarding.invite.error.already_registered` sentence — before 2026-09-25
    the machine funnelled the code into the transient arm and told the user to
    try again, for ever.

    A REAL registered-then-suspended actor, not case E's random seed: a random
    seed's submit SUCCEEDS (a pending row), so only a key the nest actually
    holds can reach the refusal under test. Native-only for case K's reason —
    the typed loopback handle is dialled directly, which a browser cannot."""
    from actions import ActionLayer
    from common.auth import _authed_call

    nest = smoke_handled_nest
    actor = register_handled_actor(
        nest["port"], handle=f"smokesusp{secrets.token_hex(3)}",
        domain=nest["authority"], base_url=nest["url"],
    )
    _authed_call(nest["url"], nest["admin"]["signing_key"], "fauna.admin.users.suspend", {
        "actor_id": actor["actor_id_bytes"],
        "category": "other",
        "reason": "e2e: suspended user's invite fallthrough",
    })
    typed_handle = f"{actor['handle']}@{nest['authority']}"

    driver = launch_harness.launch_and_route(
        secret_hex=bytes(actor["signing_key"]).hex(), node_url=nest["url"], trust=nest
    )
    app = ActionLayer(driver)
    # The refused surface (case E) — only its fallthrough matters here.
    driver.wait_for(LAUNCH_FALLTHROUGH, timeout=ROUTE_TIMEOUT)
    driver.click(LAUNCH_FALLTHROUGH)
    driver.wait_for(HANDLE_INPUT, timeout=15)
    app.onboarding.fill_handle(typed_handle)
    app.onboarding.run_handle_check(timeout=45)
    app.onboarding.submit_handle()
    driver.wait_for(INVITE_SUBMIT, timeout=ROUTE_TIMEOUT)
    driver.click(INVITE_SUBMIT)

    # A condition wait on the painted sentence (convention 14), never a fixed
    # delay: the budget is spent only by a genuine failure.
    wait_until(
        lambda: S.onboarding.invite.error.already_registered
        in (driver.get_text(INVITE_STATUS) or ""),
        ROUTE_TIMEOUT,
        diagnose=lambda: f"{INVITE_STATUS}={driver.get_text(INVITE_STATUS)!r}, "
        f"app error: {app.error_text()!r}",
    )
    status = driver.get_text(INVITE_STATUS) or ""
    assert "Try again" not in status, (
        f"an already-registered refusal never reads as a retry, got {status!r}"
    )
