"""E2E test: the unified feed-side **Bridges** page, driven through the real
client UI end-to-end against a real nest (docs/goal/behavior/bridges.md).

Zero prior coverage of this page existed before this file: no
`bridge-action-button`/`bridge-card`/`bridge-link-form` references anywhere in
`tests/e2e-unified/{actions,drivers,tests}/`. `test_nostr.py` and
`api/test_bluesky_oauth.py` exercise the *same* `fauna.bridges.*` wire contract
but through Nostr's own dedicated settings page and a raw-API OAuth shortcut
respectively — neither drives *this* page's UI.

ActivityPub's `enable` link mode is the entry vehicle: zero fields, a single
link mode, no external network call, no domain configuration required
(`bridge_provider.rs` falls back to `"localhost"`) — the only currently-live
feed-side provider whose full link flow is deterministic and fully offline.
Bluesky needs a real/faked OAuth far end (`conformance_bluesky_link.rs`); Nostr
is excluded from this page by design (bridges.md § Scope — it has its own
dedicated page, `nostr.md`).
"""

from __future__ import annotations

import json
import time
import urllib.request

import pytest

from actions.api_actor import ApiActor
from helpers.e2e_session import E2E_LOGIN_DEVICE_ID
from i18n.strings import S

pytestmark = pytest.mark.tier_3

AP_BRIDGE_ID = "activitypub"


@pytest.fixture(scope="module")
def bridges_nest_binary():
    """fauna-nest built with the `activitypub` provider (+ test-hooks) — the
    standalone build `bridges_nest` names to the provider seam, because the
    default `nest_binary` does not compile ActivityPub. See the module
    docstring for why ActivityPub is the entry vehicle.

    Leaving out `nostr`/`bluesky` saves a standalone build, and nothing more.
    The page renders one card either way: every app filters it through the
    shared `is_unified_bridges_page_bridge`, which drops both. That is why a
    docker run, whose image compiles all three providers, needs no other
    shape (`helpers/nest_surface.py::IMAGE_SERVABLE_BINARY_FIXTURES`).

    Delegates to conftest.py's memoized ensure layer so this fixture replays
    `_prebuild_binaries`'s collection-time build instead of paying a second
    slot-wait inside this module's first requesting test (build-system.md
    § Build/e2e slot locks)."""
    from conftest import _ensure_bridges_nest_built
    return _ensure_bridges_nest_built()


@pytest.fixture(scope="module")
def bridges_nest(request, nest_mode, tmp_path_factory):
    """A claimed nest (ActivityPub compiled in) with one registered actor,
    started by the run's mode provider: standalone builds
    `bridges_nest_binary` (named to the seam, a provider set the shared
    `nest_binary` doesn't build), a container run serves the shipped image
    (`testing.md` § Default app and nest mode, ruling (1))."""
    from conftest import _make_user, _start_dedicated_nest
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "bridges-nest",
        binary="bridges_nest_binary",
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def bridges_spa_url(static_dir, bridges_nest):
    """Web-only SPA proxy -> `bridges_nest`. The session `spa_url` only
    proxies the shared `nest_instance` (see that fixture in conftest.py) —
    mirrors `dedicated_mail_spa_url`."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, bridges_nest["url"])
    yield url
    server.shutdown()


def _bridges_node_url(app, bridges_nest, request):
    """The address the client's WS-RPC layer connects to for this test's
    dedicated nest: the raw nest URL for native apps, the per-test SPA
    proxy for web (the browser needs a CORS-bearing origin). Resolved lazily
    so a native-only run never instantiates the web-only proxy."""
    if app.driver.is_web():
        return request.getfixturevalue("bridges_spa_url")
    return bridges_nest["url"]


def _login_to_bridges_nest(app, bridges_nest, request):
    """Log `app` in as `bridges_nest`'s registered user — NOT the shared
    session `nest_instance`/`test_user` (mirrors `self_signed_logged_in_app`
    / `login_as_nest_admin`, but a plain registered user, not the admin).

    Exposed separately from the `bridges_logged_in_app` fixture below so a
    caller can install a `bridge_status_override` (`_install_status_override`)
    BEFORE this fires: login is what triggers the app's ONE-SHOT initial
    `fauna.bridges.list` fetch on native apps (`fauna_client.fetch_bridges()`
    at `AuthSucceeded`, `apps/fauna-linux/src/app.rs`) — linux never re-fetches
    on a bare nav-to-Bridges (only at login or after a link/unlink/settings
    mutation), so installing the override AFTER `bridges_logged_in_app` has
    already logged in races the wrong side of that one-shot fetch and the
    detail pane renders a stale, un-degraded snapshot."""
    from conftest import _relaunch_trusting_nest
    from drivers.http_bridge import HttpBridgeDriver
    _relaunch_trusting_nest(app.driver, bridges_nest)
    node_url = _bridges_node_url(app, bridges_nest, request)
    user = bridges_nest["user"]
    secret_hex = user["signing_key"].encode().hex()
    if isinstance(app.driver, HttpBridgeDriver):
        app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "e2e-user",
                "actor_id": user["actor_id_hex"],
                "device_id": E2E_LOGIN_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
    else:
        app.auth.login(
            node_url=node_url,
            username=user["actor_id_hex"],
            password=secret_hex,
            secret_hex=secret_hex,
        )
    return app


@pytest.fixture
def bridges_logged_in_app(request, app, bridges_nest):
    return _login_to_bridges_nest(app, bridges_nest, request)


def _bridges_api_actor(bridges_nest) -> ApiActor:
    user = bridges_nest["user"]
    return ApiActor(
        bridges_nest["url"], user["token"], user["actor_id_hex"],
        bytes(user["signing_key"]),
    )


def _find_bridge(bridges: list, bridge_id: str) -> dict | None:
    return next((b for b in bridges if b["id"] == bridge_id), None)


def _install_status_override(base_url: str, bridge_id: str, **payload) -> None:
    """`POST /api/v1/test/bridges/<id>/status-override`
    (`bridge_status_test_hook.rs`) — force `bridge_id`'s `provider.status()`
    into a degraded shape. `payload` is `{"error": "<msg>"}` or
    `{"no_applicable_modes": True}` — see `OverrideBody` in the Rust hook.

    Needed because every registered provider's `status()` only errors on a
    genuine DB fault, and every declared `BridgeLinkMode` today has
    `platform: None` (so a client's `applicable_modes` count never falls to
    zero on its own) — the un-linkable-mode shape
    (`fauna_client_bridges::link_block`) is otherwise unreachable from a real
    running nest."""
    req = urllib.request.Request(
        f"{base_url}/api/v1/test/bridges/{bridge_id}/status-override",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    urllib.request.urlopen(req).read()


def _clear_status_override(base_url: str, bridge_id: str) -> None:
    req = urllib.request.Request(
        f"{base_url}/api/v1/test/bridges/{bridge_id}/clear",
        data=b"",
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    urllib.request.urlopen(req).read()


@pytest.mark.feature("bridges")
def test_activitypub_links_through_the_bridges_page(bridges_logged_in_app, bridges_nest):
    """Drive the real Bridges-page UI to link ActivityPub, verify server-side.

    The mutation (the link) goes through the real client UI (point 8); the
    outcome is verified against `fauna.bridges.list` directly because
    `bridge-action-button` is the SAME id for both the link and unlink action
    (bridges.md § User actions), so there is no reliable client-side "linked"
    signal to poll for instead (see `BridgesActions` docstring).
    """
    app = bridges_logged_in_app
    app.bridges.navigate()
    assert app.bridges.is_page_visible(), (
        f"Bridges page should be reachable. error: {app.bridges.page_error_text()!r}"
    )

    before = _bridges_api_actor(bridges_nest).bridges_list()
    ap_before = _find_bridge(before, AP_BRIDGE_ID)
    assert ap_before is not None, f"activitypub should be in the bridge list: {before!r}"
    assert ap_before["available"] is True, f"activitypub should be available: {ap_before!r}"
    assert ap_before["linked"] is False, f"activitypub should start unlinked: {ap_before!r}"

    app.bridges.link(AP_BRIDGE_ID)

    # Read the page error HERE, not in the assert message 15 s later
    # (convention 6: a failure must diagnose itself). Both `LinkBridgeAsync`
    # and `LoadAsync` open with `ErrorMessage = null`, and windows'
    # `BridgesPage` propagates that null straight to `App.CurrentErrorMessage`
    # (Views/BridgesPage.xaml.cs) — so ANY refresh during the poll below erases
    # the very error that explains the failure. Reading it after the loop is
    # what made this failure evidence-free across three sessions; `timeout=0`
    # takes the current value without spending 8 s waiting for one.
    post_link_error = app.bridges.page_error_text(timeout=0)

    # Is the modal still up? On linux and windows the link action is a two-step
    # dialog ceremony (`BridgesActions.link`). Still open => the submit click
    # did not carry the dialog through its close, so
    # `BridgeCardActions.RunAsync` is still parked on `await dialog.ShowAsync()`
    # and NO link RPC was ever sent — the ceremony stalled. Closed does NOT
    # prove the RPC went out: `ShowAsync` returns on ANY close (ESC, a click
    # landing on Cancel, an external dismissal), and `RunAsync` then silently
    # skips the RPC when the confirm handler never ran (`shouldLink` stays
    # false). Splitting THAT pair — silent-cancel vs. dispatched-but-unfinished
    # RPC — takes the `FAUNA_E2E_AGENT_LOG` ceremony trace (`BridgeCardActions`
    # writes `[bridge-link] dialog closed, confirmed=…`; the VM writes
    # `rpc dispatching` / `rpc ok` / `rpc FAILED`). Always False on the 5
    # clients that submit inline (`is_visible` on an id this app never renders
    # is False on every driver).
    link_form_still_open = app.driver.is_visible("bridge-link-form")

    ap_after, slow_link_error, link_elapsed = _await_linked(app, bridges_nest, True)

    # `ap_after["error"]` is the nest's PER-BRIDGE diagnostic (bridges_ui.rs:
    # set when `provider.status()` errored, and the shape in which `link_modes`
    # comes back absent). No client renders it, so the test is the only place it
    # is ever seen — print it explicitly rather than trusting it to show up in
    # the repr.
    assert ap_after is not None and ap_after["linked"] is True, (
        f"activitypub should be linked after the UI link action "
        f"(waited {link_elapsed:.1f}s; the fauna.bridges.link envelope is 30s). "
        f"page error right after the link action: {post_link_error!r}; "
        f"first page error during the poll: {slow_link_error!r}; "
        f"page error now: {app.bridges.page_error_text(timeout=0)!r}; "
        f"link form still open right after the action: {link_form_still_open} "
        f"(now: {app.driver.is_visible('bridge-link-form')}); "
        f"nest per-bridge error: {(ap_after or {}).get('error')!r}; "
        f"link_modes: {(ap_after or {}).get('link_modes')!r}; "
        f"last status: {ap_after!r}"
    )
    assert ap_after.get("identity") is not None, (
        f"a linked activitypub bridge should carry an identity: {ap_after!r}"
    )

    # Unlink through the same UI, verify it round-trips back to unlinked.
    app.bridges.unlink(AP_BRIDGE_ID)

    # Same transient-error capture as the link half above.
    post_unlink_error = app.bridges.page_error_text(timeout=0)
    # Same split as the link half above — windows is the only client whose
    # unlink confirms through a modal (ui.yaml `platform_elements.windows`).
    unlink_modal_still_open = app.driver.is_visible("bridge-unlink-confirm-modal")

    ap_final, slow_unlink_error, unlink_elapsed = _await_linked(app, bridges_nest, False)

    assert ap_final is not None and ap_final["linked"] is False, (
        f"activitypub should be unlinked after the UI unlink action "
        f"(waited {unlink_elapsed:.1f}s). "
        f"page error right after the unlink action: {post_unlink_error!r}; "
        f"first page error during the poll: {slow_unlink_error!r}; "
        f"page error now: {app.bridges.page_error_text(timeout=0)!r}; "
        f"unlink modal still open right after the action: {unlink_modal_still_open} "
        f"(now: {app.driver.is_visible('bridge-unlink-confirm-modal')}); "
        f"nest per-bridge error: {(ap_final or {}).get('error')!r}; "
        f"last status: {ap_final!r}"
    )


@pytest.mark.feature("bridges")
def test_activitypub_link_blocked_shows_the_nests_own_reason(app, bridges_nest, request):
    """A bridge whose `provider.status()` errored (`bridges.md` § Errors &
    edge cases → *A bridge that cannot be linked right now*, ratified
    2026-08-11): `bridge-action-button` stays rendered but DISABLED, and
    `bridge-link-blocked-reason` paints the nest's own sentence VERBATIM
    beside it — never a substituted generic string, never dropped.

    Zero prior e2e coverage of this surface existed anywhere before this
    test:
    the feature is built and unit-tested on all 7 apps individually
    (e.g. `apps/fauna-tui/src/bridges.rs::
    a_degraded_bridge_disables_link_and_shows_the_nests_own_reason`), but no
    running-nest, real-app-UI journey exercised it. `bridge_status_test_hook.rs`
    is the seam: every registered provider's `status()` only errors on a
    genuine DB fault, so a running nest cannot organically reach this shape.

    Installs the override BEFORE logging in — see `_login_to_bridges_nest`'s
    docstring for why (linux's one-shot initial fetch)."""
    base_url = bridges_nest["url"]
    reason = "relay handshake failed: connection refused"
    _install_status_override(base_url, AP_BRIDGE_ID, error=reason)
    try:
        app = _login_to_bridges_nest(app, bridges_nest, request)
        app.bridges.navigate()
        assert app.bridges.is_page_visible(), (
            f"Bridges page should be reachable. error: {app.bridges.page_error_text()!r}"
        )
        # No-op on every app except linux, where `bridge-action-button` (and
        # the reason beside it) lives behind the detail pane opened by
        # activating the bridge's list row — same first step `link()`/`unlink()`
        # take (`BridgesActions._open_bridge` docstring).
        app.bridges._open_bridge(AP_BRIDGE_ID)
        app.driver.wait_for("bridge-link-blocked-reason")

        assert app.driver.is_visible("bridge-action-button"), (
            "a blocked bridge's action button must stay rendered, DISABLED — "
            "never absent (bridges.md § Errors & edge cases)"
        )
        assert app.driver.is_disabled("bridge-action-button"), (
            "a bridge with no applicable link mode must not offer a live "
            "Link control — several apps' original bug was a button that "
            "looked live and silently swallowed the tap"
        )
        assert app.driver.get_text("bridge-link-blocked-reason") == reason, (
            f"the nest's own explanation should render verbatim, not a "
            f"substituted generic string: got "
            f"{app.driver.get_text('bridge-link-blocked-reason')!r}"
        )
    finally:
        _clear_status_override(base_url, AP_BRIDGE_ID)


@pytest.mark.feature("bridges")
def test_activitypub_link_blocked_falls_back_to_the_generic_reason(app, bridges_nest, request):
    """Degraded with no `error` on the wire (`link_modes` declared empty, no
    provider fault): still blocked, but the reason falls back to the
    localized `bridges.no_link_method` string — never an empty line, never a
    live button.

    Installs the override BEFORE logging in — see `_login_to_bridges_nest`'s
    docstring for why (linux's one-shot initial fetch)."""
    base_url = bridges_nest["url"]
    _install_status_override(base_url, AP_BRIDGE_ID, no_applicable_modes=True)
    try:
        app = _login_to_bridges_nest(app, bridges_nest, request)
        app.bridges.navigate()
        assert app.bridges.is_page_visible(), (
            f"Bridges page should be reachable. error: {app.bridges.page_error_text()!r}"
        )
        app.bridges._open_bridge(AP_BRIDGE_ID)
        app.driver.wait_for("bridge-link-blocked-reason")

        assert app.driver.is_visible("bridge-action-button"), (
            "a blocked bridge's action button must stay rendered, DISABLED — "
            "never absent (bridges.md § Errors & edge cases)"
        )
        assert app.driver.is_disabled("bridge-action-button"), (
            "a bridge with no applicable link mode must not offer a live "
            "Link control"
        )
        assert app.driver.get_text("bridge-link-blocked-reason") == S.bridges.no_link_method, (
            f"no per-bridge error means the localized generic fallback, not "
            f"an empty/missing reason: got "
            f"{app.driver.get_text('bridge-link-blocked-reason')!r}"
        )
    finally:
        _clear_status_override(base_url, AP_BRIDGE_ID)


def test_bridge_card_scopes_its_members(bridges_logged_in_app, bridges_nest):
    """`bridge-card` is a real, indexed CONTAINER (ui.yaml: `type: view`,
    `indexed: true`) — not just a name shared by its member elements. A scoped
    query addresses one bridge's row directly (`scope="bridge-card[i]"`,
    e2e-conventions.md convention 1's descendant-match rule), the pattern
    `actions/bridges.py`'s `link`/`unlink` now use on every app (index is
    otherwise ambiguous once more than one provider renders here — today the
    shared `is_unified_bridges_page_bridge` filter leaves exactly one card in
    every mode, so this exercises index 0/1).

    All 7 apps now wire it: tui (`.within(ids::BRIDGE_CARD, i)`) and windows
    (a real `AutomationId="bridge-card"`) first; apple via
    `BridgeCardContent`'s `.automationScope`/self-registering presence entry; web (`data-testid={IDS.BRIDGE_CARD}` on
    `BridgeCard.svelte`'s root div), linux (`set_test_id` on
    `build_bridge_detail_content`'s root `scroll_content` box — only
    `bridge-card[0]` ever resolves there, one detail pane at a time) and
    android (`.testTag(Ids.BRIDGE_CARD)` on `BridgesScreen.kt`'s Card) closed
    the remaining three.
    """
    app = bridges_logged_in_app
    driver = app.driver

    app.bridges.navigate()
    assert app.bridges.is_page_visible(), (
        f"Bridges page should be reachable. error: {app.bridges.page_error_text()!r}"
    )
    # linux only: opens the detail pane (the only place its bridge-card
    # lives). No-op everywhere else — web/android/apple render every card
    # inline once the page loads (see `_open_bridge`'s own docstring).
    app.bridges._open_bridge(AP_BRIDGE_ID)

    driver.wait_for("bridge-action-button", scope="bridge-card[0]")
    assert driver.is_visible("bridge-action-button", scope="bridge-card[0]"), (
        "bridge-card[0] (the only bridge the unified page renders) "
        "should resolve its own bridge-action-button as a descendant scope match"
    )
    # A count: "resolves nothing" is a tree question, and a second card rendered
    # below the fold would read "not visible" on windows (!IsOffscreen) and pass
    # this vacuously -- the very scoping bug the test exists to catch.
    assert driver.count("bridge-action-button", scope="bridge-card[1]") == 0, (
        "a bridge-card index past the last rendered card should resolve nothing"
    )


def _await_linked(app, bridges_nest, want: bool, budget: float = 45.0):
    """Poll `fauna.bridges.list` until activitypub's `linked` is `want`.

    Returns `(last_status, first_client_error, elapsed_seconds)`. Deadline
    poll, never a settle-sleep: a green cycle pays only the real latency
    (convention 14 — `testing.md` § point 14).

    The 45 s default is sized to the protocol's own completion envelope, not
    generosity: `fauna.bridges.link` is registered with a **30 s** default
    deadline (`libs/fauna-protocol/src/kind.rs`), and the client request path
    silently waits out a WS reconnect INSIDE that budget before even sending
    (`libs/fauna-client/src/client.rs::request_inner`). The pre-2026-08-01
    15 s budget was therefore a budget INVERSION: a link that lands — or
    errors — at t=16–30 s is protocol-legal, yet the poll declared it a lost
    RPC while reading the error channel before any slow error could exist.

    `first_client_error` is the first non-empty state-protocol error seen
    DURING the poll: a slow-failing RPC surfaces its error at up to 30 s and
    any later refresh wipes it (`LoadAsync` opens with `ErrorMessage = null`),
    so sampling every iteration is the only read that cannot miss it.
    """
    start = time.monotonic()
    deadline = start + budget
    last = None
    first_error = ""
    while time.monotonic() < deadline:
        last = _find_bridge(_bridges_api_actor(bridges_nest).bridges_list(), AP_BRIDGE_ID)
        if last and last["linked"] is want:
            return last, first_error, time.monotonic() - start
        if not first_error:
            try:
                first_error = str(app.driver.get_state("messages.error") or "")
            except Exception:
                pass
        time.sleep(0.5)
    return last, first_error, time.monotonic() - start


@pytest.mark.windows
def test_activitypub_link_cycles_are_not_flaky(bridges_logged_in_app, bridges_nest):
    """Drive the link/unlink ceremony repeatedly — the flake hunter.

    `test_activitypub_links_through_the_bridges_page` performs the ceremony
    ONCE per ~17 s run, which is a terrible sampling rate for an intermittent
    failure: three sessions chased it by re-running the whole test and only
    ever caught green. This runs the identical user path back to back, so a
    single invocation samples the seam an order of magnitude more often.

    Beyond sampling rate it carries one assertion the single-shot test does not:
    that each ceremony leaves NO modal behind. That is both a real product
    assertion and what keeps the `still open` diagnostics honest — see the
    assertion sites below.

    **Hunting the flake:** the default cycle count is deliberately small so this
    costs the windows suite ~20 s rather than minutes. Raise it with
    `FAUNA_E2E_BRIDGE_CYCLES=40` (~4 min, 80 ceremonies) when actively chasing
    the intermittent link failure, and do it on a LOADED machine — 92 ceremonies
    on a quiet one (2026-08-01) proved nothing, which is exactly the trap
    already flagged. Three practicalities the hunt learned the
    hard way:

    * Wrap pytest in the machine-wide 'e2e' slot so a sibling's build can BE
      the load instead of starving this run's `_ensure_app_built` no-op check
      (conftest's `FAUNA_SLOT_HELD_E2E` re-entrancy runs under your slot).
      ⚠ Never wrap pytest in the 'build' pool instead — that shape held both
      build slots while queued for e2e and deadlocked the machine 2026-08-18;
      the e2e-slot acquisition refuses it now.
    * Set `FAUNA_E2E_AGENT_LOG=<path>` so the app writes the ceremony trace
      (`[bridge-link] dialog closed, confirmed=…` / `rpc dispatching` /
      `rpc ok` / `rpc FAILED`) — on a failing cycle it is the only evidence
      that splits a silently-cancelled dialog from a dispatched-but-lost RPC.
    * No sibling available? A bounded synthetic-CPU-load generator can stand
      in without touching the build-slot system.

    This test multiplies CEREMONY samples, not app-launch samples — the app
    and its WS connection are set up once per run, so a per-launch trigger
    (cold connect, first page load) is sampled once no matter the cycle count;
    the single-shot test above is what samples those.
    """
    import os
    cycles = int(os.environ.get("FAUNA_E2E_BRIDGE_CYCLES", "3"))

    app = bridges_logged_in_app
    app.bridges.navigate()
    assert app.bridges.is_page_visible(), (
        f"Bridges page should be reachable. error: {app.bridges.page_error_text()!r}"
    )
    start_status, _, _ = _await_linked(app, bridges_nest, False, 5.0)
    assert start_status and start_status["linked"] is False, (
        "activitypub should start unlinked"
    )

    for i in range(cycles):
        app.bridges.link(AP_BRIDGE_ID)
        link_err = app.bridges.page_error_text(timeout=0)
        link_form_open = app.driver.is_visible("bridge-link-form")
        linked, slow_link_err, link_elapsed = _await_linked(app, bridges_nest, True)
        # Latency history lands in captured stdout, so ANY later failure shows
        # whether land-times were creeping toward the 30 s envelope first —
        # the signature of the reconnect-wait/slow-RPC branch.
        print(f"cycle {i}: link landed in {link_elapsed:.1f}s")
        assert linked is not None and linked["linked"] is True, (
            f"cycle {i}: LINK did not land within the 30s protocol envelope "
            f"(+15s margin). "
            f"page error right after the action: {link_err!r}; "
            f"first page error during the poll: {slow_link_err!r}; "
            f"link form still open right after the action: {link_form_open} "
            f"(now: {app.driver.is_visible('bridge-link-form')}); "
            f"nest per-bridge error: {(linked or {}).get('error')!r}; "
            f"link_modes: {(linked or {}).get('link_modes')!r}; "
            f"last status: {linked!r}"
        )
        # A completed ceremony leaves no modal behind. This is a real product
        # assertion (a dialog that outlives its own submit is a bug), and it is
        # also what KEEPS the `link form still open` diagnostic above honest: if
        # a dismissed ContentDialog left its content in the UIA tree, that
        # boolean would read True on every failure regardless of cause and
        # quietly mislead the next session. Pinning it on the green path is what
        # makes it trustworthy on the red one.
        assert app.driver.is_absent("bridge-link-form"), (
            f"cycle {i}: the link modal is still up after the link landed"
        )

        app.bridges.unlink(AP_BRIDGE_ID)
        unlink_err = app.bridges.page_error_text(timeout=0)
        unlink_modal_open = app.driver.is_visible("bridge-unlink-confirm-modal")
        unlinked, slow_unlink_err, unlink_elapsed = _await_linked(app, bridges_nest, False)
        print(f"cycle {i}: unlink landed in {unlink_elapsed:.1f}s")
        assert unlinked is not None and unlinked["linked"] is False, (
            f"cycle {i}: UNLINK did not land. "
            f"page error right after the action: {unlink_err!r}; "
            f"first page error during the poll: {slow_unlink_err!r}; "
            f"unlink modal still open right after the action: {unlink_modal_open} "
            f"(now: {app.driver.is_visible('bridge-unlink-confirm-modal')}); "
            f"nest per-bridge error: {(unlinked or {}).get('error')!r}; "
            f"last status: {unlinked!r}"
        )
        assert app.driver.is_absent("bridge-unlink-confirm-modal"), (
            f"cycle {i}: the unlink modal is still up after the unlink landed"
        )


def _feed_link_state(ward_rpc) -> tuple[bool, list[dict]]:
    """`(activitypub linked?, the ward's own link asks)` as the nest sees them —
    `fauna.bridges.list` and the supervised side of `fauna.family.status`."""
    bridge = _find_bridge(ward_rpc.call("fauna.bridges.list", {}).get("bridges", []), AP_BRIDGE_ID)
    asks = [
        r for r in ward_rpc.call("fauna.family.status", {}).get("feed_requests") or []
        if r["bridge_id"] == AP_BRIDGE_ID and r["operation"] == "link"
    ]
    return bool((bridge or {}).get("linked")), asks


@pytest.mark.feature("family-safety")
def test_family_ward_feed_source_ask_redeems_once(app, bridges_nest, request):
    """family-safety.md § Feed-source approvals, end to end on the Bridges page:
    a `feed_sources = block` ward's link is refused, the refusal offers the ask
    under that bridge's own `bridge-card`, the guardian approves it from the
    queue, the card reads "Approved — try again", and the grant is redeemed
    EXACTLY ONCE by the ward's own manual retry.

    Four properties no unit test reaches:

    1. **The typed refusal is recognised live** — the ask appears only on the
       nest's `guardian_approval_required`, and the refusal still lands on
       `error-message` (the link did not happen).
    2. **The pending state is the nest's**: the ask is a real
       `fauna.family.feed_source.request`, visible in the ward's own status.
    3. **The app never auto-retries.** After the approval the card shows the
       prompt and the bridge is STILL unlinked with the grant still unspent —
       an app that retried on seeing the approval would burn the grant on a
       render the ward never asked for.
    4. **Single-use.** The manual retry links and consumes the grant; after an
       unlink the very same link is refused again.

    `link`, not `follow`: the grant flow is one mechanism for all three
    operations, and ActivityPub's zero-field `enable` link is this page's only
    fully offline one (module docstring). A follow would also need its id
    field, which carries no ui.yaml id on any app.

    Runs on the dedicated ActivityPub nest, so this test admits its own pair
    there (one invite-request submit, on that nest's own budget). The knob is
    set over the wire as the guardian — fixture setup (point 8); the guardian's
    policy editor is `test_family.py`'s subject. The ask, the approval and the
    retry are real taps.
    """
    from clients._ws_rpc_core import RpcCallError
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _login_app_as
    from helpers.waiting import wait_until
    from tests.test_family import (
        _DEFAULT_WARD_POLICY,
        _admit_adult_directly,
        _admit_ward,
        _unique_suffix,
    )

    app.bridges.require_source_request_supported()

    suffix = _unique_suffix()
    guardian = _admit_adult_directly(bridges_nest, f"feedask-guardian-{suffix}")
    ward = _admit_ward(bridges_nest, guardian, f"feedask-ward-{suffix}")
    ward_actor = bytes.fromhex(ward["actor_id_hex"])

    def rpc_as(identity):
        return WsRpcAdminClient(
            bridges_nest["url"],
            bytes.fromhex(identity["actor_id_hex"]),
            bytes(identity["signing_key"]),
        )

    with rpc_as(guardian) as g:
        g.call(
            "fauna.family.policy.update",
            {
                "supervised_actor_id": ward_actor,
                "policy": {**_DEFAULT_WARD_POLICY, "feed_sources": "block"},
            },
        )

    def login(identity):
        app.driver.reset()
        _login_app_as(app, request, bridges_nest, identity, spa_url_fixture="bridges_spa_url")

    def states():
        return app.bridges.source_request_states(AP_BRIDGE_ID)

    # ── The ward: the link is refused, and the refusal offers the ask.
    login(ward)
    app.bridges.navigate()
    assert app.bridges.is_page_visible(), f"bridges page did not load: {app.error_text()!r}"
    assert not app.bridges.source_request_offered(AP_BRIDGE_ID), (
        "the ask must not be offered before anything was refused"
    )
    app.bridges.link(AP_BRIDGE_ID)
    wait_until(
        lambda: app.bridges.source_request_offered(AP_BRIDGE_ID),
        15.0,
        diagnose=lambda: (
            "the refused link offered no ask — is the typed refusal recognised? "
            f"error {app.bridges.page_error_text(timeout=0)!r}; "
            f"{app.driver.diagnose('bridge-source-request-button')}"
        ),
    )
    assert S.bridges.source_blocked in app.bridges.page_error_text(), (
        "the refusal must stay on error-message — the link did not happen"
    )
    with rpc_as(ward) as w:
        linked, asks = _feed_link_state(w)
    assert not linked and not asks, f"the refused link left state: {linked=} {asks=}"

    app.bridges.request_source(AP_BRIDGE_ID)
    wait_until(
        lambda: states() == [S.bridges.source_request_pending],
        15.0,
        diagnose=lambda: f"states {states()!r}; error {app.bridges.page_error_text(timeout=0)!r}",
    )
    assert not app.bridges.source_request_offered(AP_BRIDGE_ID), (
        "the ask button must not survive a landed ask — tapping it again re-asks"
    )
    with rpc_as(ward) as w:
        _linked, asks = _feed_link_state(w)
    assert len(asks) == 1 and asks[0].get("approved_at") is None, (
        f"the ask is not a pending row in the ward's own status: {asks!r}"
    )

    # ── The guardian approves it from the queue (a fresh guardian: one row).
    login(guardian)
    app.family.navigate()

    def queue_rows() -> int:
        n = app.family.approval_count()
        if n == 0:
            app.family.reload()
        return n

    wait_until(
        queue_rows,
        15.0,
        diagnose=lambda: f"the ask never reached the queue; error {app.error_text()!r}",
    )
    rows = [app.family.approval_text(i) for i in range(app.family.approval_count())]
    assert len(rows) == 1 and rows[0].strip(), (
        f"expected exactly this ward's ask, rendered non-blank, in a fresh guardian's queue: {rows!r}"
    )
    # family-safety.md § Feed-source approvals — the card names what Approve
    # grants (the bridge and the operation; a `link` has no target), never
    # only the ward's display-only label.
    assert AP_BRIDGE_ID in rows[0] and "link" in rows[0], (
        f"the feed_source row does not name the grant's bridge and operation: {rows!r}"
    )
    app.family.approve(0)
    with rpc_as(ward) as w:
        wait_until(
            lambda: (lambda a: len(a) == 1 and a[0].get("approved_at") is not None)(
                _feed_link_state(w)[1]
            ),
            15.0,
            diagnose=lambda: f"no grant minted; guardian error {app.error_text()!r}",
        )

    # ── The ward: the prompt, not a retry.
    login(ward)
    app.bridges.navigate()
    wait_until(
        lambda: states() == [S.bridges.source_request_approved],
        15.0,
        diagnose=lambda: f"states {states()!r}",
    )
    with rpc_as(ward) as w:
        linked, asks = _feed_link_state(w)
    assert not linked and len(asks) == 1, (
        "the app retried on its own — the approval is a grant the WARD redeems by "
        f"retrying, never the app: {linked=} {asks=}"
    )

    app.bridges.link(AP_BRIDGE_ID)
    with rpc_as(ward) as w:
        wait_until(
            lambda: _feed_link_state(w) == (True, []),
            15.0,
            diagnose=lambda: (
                f"the retry did not link and spend the grant: {_feed_link_state(w)!r}; "
                f"error {app.bridges.page_error_text(timeout=0)!r}"
            ),
        )
        # Single-use: unlink, and the very same link is refused again.
        w.call("fauna.bridges.unlink", {"bridge_id": AP_BRIDGE_ID})
        with pytest.raises(RpcCallError) as refused:
            w.call(
                "fauna.bridges.link",
                {"bridge_id": AP_BRIDGE_ID, "mode": "enable", "params": {}},
            )
    assert "guardian_approval_required" in refused.value.code, (
        "a second link after the grant was spent was not refused by the guardian "
        f"gate: {refused.value.code!r}"
    )
