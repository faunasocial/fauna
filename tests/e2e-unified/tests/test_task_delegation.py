"""Cross-app Task delegation — the per-kind runner + assignment surface.

``docs/goal/behavior/participants.md`` § Task delegation (Q-B/Q-C) and
§ The assignment picker; ``tests/e2e-unified/ui.yaml`` ``task-delegation`` page.

Heavy background task kinds run on exactly one (advisory) capable participant,
chosen by the policy order — always-on nest → plugged-in desktop → **never** a
battery mobile (the work queues instead). This page is where a person sees that
choice and overrides it.

Everything the page renders comes from one shared layer: the pure
``fauna_core::delegation::delegation_rows`` composes the user's pins
(the ``fauna.state.delegation`` plane) with the live advisory lease
(``fauna.delegation.observe``) into rows, and the async
``fauna_client_delegation::TaskDelegationView`` orchestrates the reads and
writes the pin back through the plane's read-modify-write. No client
re-derives any of it (priority #2).

tier_3: every binary is real — a real nest, a real sealed plane write, real
``fauna.delegation.observe``. Nothing is stubbed, and the mutation under test
(picking an assignment) is driven through the client UI, not an API shortcut.
"""

import secrets
import time

import pytest

from actions.task_delegation import AUTOMATIC, THIS_DEVICE
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import app_name, skip_unbuilt

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# i18n `task_delegation.*` (i18n/strings/en.yaml) — the strings a user reads.
KIND_BACKUP_UPLOAD = "Backup uploads"
KIND_CONTENT_RESCORE = "Content re-scoring"
KIND_INDEX = "Search indexing"
RUNNER_THIS_DEVICE = "Running on this device"
RUNNER_WAITING = "Waiting for an eligible device"

# Which heavy task kind each app ships a runner for, and is therefore a legal
# self-pin target for (participants.md § The assignment picker — the picker rule
# is per-(client, kind)). This mirrors `HeavyTaskCapability::runner_for([…])` at
# each app's call site; keep the two in step.
#
# Absent on purpose: web and the mobiles run nothing (web additionally cannot
# ever build the content index — tantivy in a browser), so they offer no
# self-pin at all and are excluded by marker, not by a lookup miss.
RUNS = {
    "windows": KIND_INDEX,          # FfiHeavyTaskCapability::IndexOnly; upload driver deleted 2026-08-16
    "macos": KIND_INDEX,            # FfiHeavyTaskCapability::IndexOnly; upload driver deleted 2026-08-15
    "linux": KIND_INDEX,            # content-index builder; upload driver deleted 2026-07-29
    "tui": KIND_INDEX,              # content-index builder
}


class _Seats:
    """The app under test plus any further seats of the SAME fresh identity a
    delegation journey needs: the user's other devices.

    **Which app a further seat is follows from what the app under test can do.**
    Only an app in ``RUNS`` can *make* an assignment: the shared picker offers
    ``This device`` exactly where this build ships a runner for the kind, and it
    never offers another participant as a fresh target (``PinOption::Other`` is
    rendered only for a pin that already exists; ``participants.md`` § The
    assignment picker). So on a desktop column the other device is a second
    launch of that same app — the journey is then about the column end to end.
    On web and the mobiles, which run nothing, it is a **tui** seat: the user's
    computer, which tui models on every machine this suite runs on. What the
    viewer column owes is the rendering and the escape (a foreign pin is shown
    and can be cleared), and that part is the app under test's own.

    **What makes them different participants.** Every native launch gets its
    own per-launch world (convention 10 — fresh config/data dirs, credential
    store, agent port), and a seat's lease identity is this account's sync
    device id resolved from *that* world, so two launches mint two device ids
    without being told to, exactly as two real machines would.

    **A fresh actor, not ``test_user``.** The lease map and the
    ``fauna.state.delegation`` plane are both per-actor, and the session identity is shared by the
    whole run: seats of it would contend for its ``index`` lease and write its
    assignments, perturbing every later test that reads either. A fresh actor on
    the session nest is complete isolation for the cost of one registration.
    """

    def __init__(self, request, nest_instance, observer, user):
        self._request = request
        self._nest = nest_instance
        self.observer = observer
        self.user = user
        self._drivers: dict[str, object] = {}

    @property
    def observer_runs_index(self) -> bool:
        """Whether the app under test ships the content-index builder — and so
        is itself a plugged-in desktop contending for the ``index`` lease."""
        return app_name(self.observer.driver) in RUNS

    @property
    def pinning_app(self) -> str:
        """The app a seat that MAKES a pin runs: this one where it can, else tui."""
        name = app_name(self.observer.driver)
        return name if name in RUNS else "tui"

    def launch(self, app: str, tag: str):
        """Launch another seat of ``app``, logged in as this journey's user.

        Returns ``(seat, stop)``. ``stop`` tears that seat's app down once,
        idempotently — for a journey whose device has to *go away*: a live seat
        renews its lease on its next tick, which would walk a row back out of
        *Waiting* underneath the assertion.
        """
        from actions import ActionLayer
        from conftest import _build_app_config
        from drivers import create_driver

        config = _build_app_config(app, self._nest, self._request)
        driver = create_driver(app)
        # A COPY per launch: the driver stores launch state on the dict it is
        # given, and two seats sharing one would put them in one world.
        driver.launch(dict(config))
        self._drivers[tag] = driver
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": self._nest["url"],
                "secret_hex": self.user["signing_key"].encode().hex(),
                "handle": self.user["handle"],
                "actor_id": self.user["actor_id_hex"],
                "device_id": f"test-device-seat-{tag}",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })

        def stop() -> None:
            self._stop(tag)

        return ActionLayer(driver), stop

    def _stop(self, tag: str) -> None:
        driver = self._drivers.pop(tag, None)
        if driver is None:
            return
        try:
            driver.screenshot(f"teardown-seat-{tag}")
        except Exception:
            pass
        driver.teardown()

    def stop_all(self) -> None:
        for tag in list(self._drivers):
            self._stop(tag)


@pytest.fixture
def delegation_seats(request, app, nest_instance):
    """The app under test, logged in as a DEDICATED fresh actor, plus a
    launcher for that actor's other devices (:class:`_Seats`).

    Convention 16's shape — N seats on one machine, no operator, no ``go``-wait —
    at the smallest count that states each fact: "the same on every device" and
    "waits for that device rather than moving to another" are both pairwise, and
    neither is stateable from one seat. tui seats launched for a viewer column
    are prebuilt outside the test's budget (``_CROSS_APP_FIXTURE_APPS``).
    """
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    # `verify_live_actor=True`: this fixture's whole premise is a dedicated
    # actor, and a silently-declined switch would leave the app reading the
    # previous test's assignments (the `ungranted_app` rule).
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    seats = _Seats(request, nest_instance, app, user)
    try:
        yield seats
    finally:
        seats.stop_all()


def _offered(td, row: int, app) -> list[str]:
    """The picker's whole painted option set, or a declared skip where this
    app's bridge cannot report it yet.

    Absence of an option is the assertion these journeys make, and a bridge
    that does not serve the ``"options"`` attr cannot observe absence at all —
    so it must neither pass nor read as a product failure. It is automation-
    surface debt, tallied as such (convention 7) and failing under
    ``--strict-app``.
    """
    keys = td.option_keys(row)
    if keys is None:
        skip_unbuilt(
            app.driver,
            surface="the `options` attr on task-delegation-assignment-picker",
            detail="the bridge must report every option a picker paints "
            "(`drivers/base.py::option_texts`)",
            tracked="",
        )
    return keys


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("task-delegation")
def test_task_delegation_page_lists_the_live_task_kinds(logged_in_app):
    """The page renders one row per live task kind, each with a resolved name, a
    runner line, and an assignment picker defaulting to Automatic (zero
    configuration ⇒ the pure policy order — works out of the box)."""
    td = logged_in_app.task_delegation
    td.navigate()
    assert td.is_page_visible(), (
        f"task-delegation sub-page did not render; error={logged_in_app.error_text()!r}"
    )

    # `LIVE_TASK_KINDS` is backup-upload + the nest-run content-rescore + the
    # client-run index kind; the row order is the canonical list order, identical
    # on every app (priority #1/#3).
    assert td.row_count() == 3, (
        f"expected one row per live task kind; names={td.kind_names()!r} "
        f"error={logged_in_app.error_text()!r}"
    )
    assert td.kind_names() == [KIND_BACKUP_UPLOAD, KIND_CONTENT_RESCORE, KIND_INDEX]

    # The runner cell must render a *resolved* i18n string, never a raw key or a
    # blank. Which one depends on whether this device happens to hold the
    # advisory lease right now, so assert membership rather than a single value.
    runner = td.runner_text(0)
    assert runner in {RUNNER_THIS_DEVICE, RUNNER_WAITING} or runner.startswith("Running on "), (
        f"runner cell did not render a resolved runner string: {runner!r}"
    )

    assert td.assignment_key(0) == AUTOMATIC, (
        f"a fresh config must read as Automatic; got {td.assignment_key(0)!r}"
    )


@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.real_conversations
@pytest.mark.feature("task-delegation")
def test_the_index_row_names_this_device_as_builder_of_record(logged_in_app):
    """A logged-in builder **heartbeats the advisory ``index`` lease for the
    lifetime of its attachment**, so the row names this device — not ``Waiting``.

    ``participants.md`` § Coordination primitive → *The ``index`` kind under the
    lease* (RULED 2026-08-03): the heartbeat is attachment-lifetime, not
    while-building, and that is precisely what makes ``RunnerStatus`` truthful —
    "a builder that heartbeated only while actively staging would fall back to
    ``Waiting`` in steady state, telling the user no builder device is live when
    one is". The doc's own success line for the UniFFI seats says the row must
    "name it as the builder-of-record instead of reading Waiting-while-running".

    This is the assertion the page's other test deliberately does **not** make:
    it accepts ``{THIS_DEVICE, WAITING}`` as a set, because the *rendering* is
    what it is pinning. Nothing pinned the seat itself, on any app — which is how
    macOS and windows shipped a builder with no seat at all for two weeks
. One device is contending here, so the outcome is
    not a race: an eligible seat that heartbeats wins its own uncontested lease,
    and reading ``Waiting`` means no seat is contending at all.

    Apps here are the ones that both build the index **and** seat it — all four
    desktops since ``windows`` joined (its call site passes
    ``ISecretStore.LoadDeviceId()`` rather than ``indexLeaseDevice: null``). The
    mobiles and web never build, so they have nothing to seat (the ratified
    build-vs-query split) — a structural absence, not a skip.

    ⚠ **``real_conversations`` is a real precondition here, not conversations
    scope creep** — and it was implicit until 2026-08-24. The seat starts inside
    ``IndexBuilderLauncher::launch()`` (``fauna-client-conversations`` →
    ``index_lease::start``), which runs in the **prologue of
    ``start_receive_loop``** (``fauna-conversations/src/session.rs``). linux and tui
    start that loop at every e2e login, so they seat unconditionally; the three
    UniFFI apps gate it behind the launch-time ``FAUNA_E2E_REAL_CONVERSATIONS``
    (windows ``App.xaml.cs`` ``BuildE2eConvSessionAsync``; macOS/iOS
    ``FaunaMacApp``/``FaunaApp`` ``FaunaE2E.realConversations`` →
    ``conversationsVM.activate``). So on windows and macOS this test **cannot**
    pass without the marker — measured on windows 2026-08-24: red without it
    ("Waiting for an eligible device"), green with it, same binary. macOS's
    2026-08-17 green therefore rode a sibling module's marker flipping the
    session-wide env, which is exactly the silent dependency the conftest's
    ``enable_real_faunamls`` docstring warns about. Same reason
    ``test_search_local_index.py`` carries it: anything observing the index arm
    needs the loop that attaches it. Consequence, per that same docstring: run
    this module in its **own** pytest invocation — the env is session-wide, so
    mixing it with mock-inject DM tests would flip those to real too.
    """
    td = logged_in_app.task_delegation
    td.navigate()
    assert td.is_page_visible(), (
        f"task-delegation sub-page did not render; error={logged_in_app.error_text()!r}"
    )

    names = td.kind_names()
    assert KIND_INDEX in names, f"{KIND_INDEX!r} row absent; page rendered {names!r}"
    row = names.index(KIND_INDEX)

    runner = td.wait_for_runner(row, RUNNER_THIS_DEVICE)
    assert runner == RUNNER_THIS_DEVICE, (
        f"the {KIND_INDEX!r} row reads {runner!r}, so this login is not "
        f"heartbeating the advisory lease. {RUNNER_WAITING!r} means no seat is "
        "contending at all — on a UniFFI app the usual cause is a null "
        "`index_lease_device` reaching `conversations_session` (the device id is "
        "app-owned state the FFI factory cannot derive); on tui/linux it means "
        "the device-id store would not open, so `IndexLeaseSeat` was never built. "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.web
@pytest.mark.feature("task-delegation")
def test_web_names_the_other_device_building_the_index(logged_in_app, nest_instance, test_user):
    """The web column's leg of "a device that is building the index is named as
    doing so": web never builds the index (``participants.md`` § The assignment
    picker — it is a viewer and ships no runner), so the builder is always
    ANOTHER of the user's devices, and what web owes is naming it on the row
    (§ UI surfaces — the page shows each kind's current runner).

    The builder here is a stand-in device holding the ``index`` lease through
    the same ``fauna.delegation.heartbeat`` a builder seat sends for the
    lifetime of its attachment (§ Coordination primitive → *The ``index`` kind
    under the lease*), re-sent on every poll so it stays live well inside
    ``LEASE_STALE_MS``. Everything asserted after it — the observe, the shared
    ``runner_label`` decision, the render — is web's own. The stand-in is not
    in the device roster, so the row names it by the shared short-id fallback.
    """
    builder = secrets.token_hex(16)
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )

    def _heartbeat():
        with client:
            client.call(
                "fauna.delegation.heartbeat",
                {
                    "task_kind": "index",
                    "holder": {"Device": {"device_id": builder}},
                    "holder_class": "PluggedInDesktop",
                },
            )

    td = logged_in_app.task_delegation
    _heartbeat()
    td.navigate()
    assert td.is_page_visible(), (
        f"task-delegation sub-page did not render; error={logged_in_app.error_text()!r}"
    )
    names = td.kind_names()
    assert KIND_INDEX in names, f"{KIND_INDEX!r} row absent; page rendered {names!r}"
    row = names.index(KIND_INDEX)

    # Deadline poll on the rendered state (convention 14); re-navigating re-runs
    # the page's load, exactly as `wait_for_runner` does.
    deadline = time.monotonic() + 90.0
    runner = ""
    while time.monotonic() < deadline:
        runner = td.runner_text(row)
        if runner.startswith("Running on ") and builder[:6] in runner:
            break
        _heartbeat()
        td.navigate()
        time.sleep(0.5)
    assert runner.startswith("Running on ") and builder[:6] in runner, (
        f"the {KIND_INDEX!r} row must name the device building the index "
        f"({builder[:8]}…); it reads {runner!r}. error={logged_in_app.error_text()!r}"
    )
    assert runner != RUNNER_THIS_DEVICE, (
        "web never builds the index, so its row must never claim this device"
    )


@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.tui
@pytest.mark.feature("pin-a-job-to-this-device")
def test_pinning_the_kind_this_app_runs_to_this_device_persists(delegation_seats):
    """Pinning a kind to this device writes the ``fauna.state.delegation`` plane and survives a reload; clearing the pin returns to Automatic.

    **Which kind is pinned is per-app, because "This device" is offered exactly
    where this build ships a runner for that kind** — the picker rule is
    per-(client, kind) since 2026-08-03 (participants.md § The assignment
    picker). **Every app in ``RUNS`` is now on ``index``**: the segment-backup
    slice-5 flip finished 2026-08-16, so no app ships an in-app ``backup-upload``
    driver any more (linux 2026-07-29, android + macOS 2026-08-15, windows
    2026-08-16 — the last), each retracted its ``backup-upload`` declaration in
    the same change, and the kind itself is now ``client_runnable: false`` with
    the source nest as writer. web and the mobiles run nothing and so are absent
    entirely — a structural platform capability, not a skippable difference.

    ⚠ Do not "restore" a ``KIND_BACKUP_UPLOAD`` entry here: that row no longer
    offers ``This device`` on any app, so the pin below would never take.

    The CAS-write behaviour under test is app-independent; only the row it is
    exercised on differs. An app missing from ``RUNS`` **fails** rather than
    skipping: a silent skip here would read as coverage the run never had.

    On a fresh actor: assignments are account-wide and outlive the test, so on
    the shared identity one column's failed run left its pin for the next
    column to start from.
    """
    logged_in_app = delegation_seats.observer
    td = logged_in_app.task_delegation
    td.navigate()
    assert td.is_page_visible(), "task-delegation sub-page did not render"

    app = app_name(logged_in_app.driver)
    assert app in RUNS, (
        f"{app!r} is not in RUNS — declare which heavy task kind it ships a "
        "runner for (participants.md § The assignment picker), or drop its "
        "marker from this test"
    )
    kind = RUNS[app]
    names = td.kind_names()
    assert kind in names, f"{kind!r} row absent; page rendered {names!r}"
    row = names.index(kind)

    assert td.assignment_key(row) == AUTOMATIC

    td.set_assignment(row, "this-device")
    assert td.wait_for_assignment(row, THIS_DEVICE), (
        f"picker did not take the pin; got {td.assignment_key(row)!r} "
        f"error={logged_in_app.error_text()!r}"
    )

    # Re-navigating re-reads the pins off the account store — so this asserts
    # the pin was stored, not just painted on the widget.
    td.navigate()
    assert td.is_page_visible()
    assert td.wait_for_assignment(row, THIS_DEVICE), (
        "the pin did not persist across a reload — the write never reached the "
        f"account store; got {td.assignment_key(row)!r} error={logged_in_app.error_text()!r}"
    )

    # Clearing the pin removes the assignment row entirely (`set_pin(kind, None)`),
    # returning the kind to the automatic policy order.
    td.set_assignment(row, "automatic")
    assert td.wait_for_assignment(row, AUTOMATIC), (
        f"unpin did not take; got {td.assignment_key(row)!r}"
    )
    td.navigate()
    assert td.is_page_visible()
    assert td.wait_for_assignment(row, AUTOMATIC), (
        f"unpin did not persist; got {td.assignment_key(row)!r} "
        f"error={logged_in_app.error_text()!r}"
    )


# ── The policy order, the picker's legal set, and the two departures ──────
#
# Five outcomes the page's first four tests deliberately did not assert, all
# `[app]` (`docs/features/task-delegation.md` outcomes 4–8). They are grouped
# here because they share two seams neither of the earlier tests needed: a
# participant that is not this app (another seat of the same user, or
# `helpers/delegation_lease.heartbeat` — the nest, a phone, an unrostered
# device), and a lease that has **gone stale** (`…age_past_stale`, the nest's
# `test-hooks` age seam, so the journeys that take it are standalone-nest only,
# convention 15). Both are fixture-setup arrangements of the world; every
# assignment these journeys *make* still goes through a real picker
# (convention 8).
#
# One journey per outcome on all 7 apps. What differs per column is declared,
# never skipped around: `RUNS` says which apps contend for the `index` lease
# themselves, and `_Seats` picks the app a pinning seat runs.


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("task-delegation")
def test_only_a_kind_this_device_can_run_offers_this_device(delegation_seats):
    """**Outcome 6** — *a job can only be assigned to a device that can actually
    run it*.

    The picker's option list is a **correctness surface**
    (``participants.md`` § The assignment picker): a pin collapses the candidate
    set to exactly the pinned participant, "so pinning a participant that can
    *never* be eligible makes the task kind wait forever". The rule is
    per-(client, kind) and has two halves, both of which must hold for
    ``This device`` to be offered — ``TaskKindSpec::client_runnable`` (can *any*
    client run this kind) and ``HeavyTaskCapability::runs`` (does *this build*
    ship a runner for it).

    So the expected set is per app, read from ``RUNS``: a desktop that ships the
    content-index builder offers a self-pin on ``index`` and on nothing else
    (``content-rescore`` "offers 'This device' on **no** client, desktop
    included", and ``backup-upload``'s transitional exception "is OVER as of
    2026-08-16"); web and the mobiles run nothing, so they offer it on **no**
    row. The assertion inverts there rather than being skipped — a viewer that
    offered a self-pin would strand the kind on a device that can never run it.

    ``test_pinning_the_kind_this_app_runs_to_this_device_persists`` pins
    ``index`` and reads back ``index``, which a picker that wrongly offered
    ``This device`` everywhere would pass identically — so the negative half is
    the whole point here, and asserting an option's **absence** needs the whole
    painted set, not the selected value.

    **On a fresh actor, because the option set is only the legal set while
    nothing is pinned.** An assignment the user already holds is always
    rendered, even where it would not be offered (the picker's escape
    corollary), so a pin some earlier test left on the shared identity shows up
    here as an option this app never offers — and reads as exactly the bug this
    test exists to catch.
    """
    logged_in_app = delegation_seats.observer
    td = logged_in_app.task_delegation
    td.navigate()
    assert td.is_page_visible(), (
        f"task-delegation sub-page did not render; error={logged_in_app.error_text()!r}"
    )

    offered = {
        name: _offered(td, td.row_of(name), logged_in_app) for name in td.kind_names()
    }

    # Automatic is offered first on every kind and every app — the
    # zero-configuration default and the escape from any pin.
    for name, keys in offered.items():
        assert keys and keys[0] == AUTOMATIC, (
            f"the {name!r} picker must offer Automatic first; it painted {keys!r}"
        )

    app = app_name(logged_in_app.driver)
    runs = RUNS.get(app)
    for name, keys in offered.items():
        if name == runs:
            assert THIS_DEVICE in keys, (
                f"{app} ships the runner for {name!r}, so that row must offer a "
                f"self-pin; it painted {keys!r}. Without it the user cannot "
                f"assign the one kind this device actually runs."
            )
        else:
            assert THIS_DEVICE not in keys, (
                f"{app} ships no runner for {name!r}"
                + ("" if runs else " (it runs no heavy task kind at all)")
                + ", so it may not offer a self-pin — taking that option would "
                f"collapse the candidate set to a participant that can never run "
                f"the kind and strand it forever. The picker painted {keys!r}."
            )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.real_conversations
@pytest.mark.feature("task-delegation")
def test_on_automatic_the_policy_prefers_the_nest_then_this_desktop_never_a_phone(
    delegation_seats, nest_instance
):
    """**Outcome 5** — *left on automatic, a job runs on your always-on nest when
    it can, otherwise on a plugged-in computer, and never on a phone or tablet —
    it waits instead*.

    ``participants.md`` § Concepts states the policy order as one sentence: "a
    task kind runs on: (1) a nest holding a sufficient grant for it, else (2) a
    plugged-in desktop, else (3) it waits. **Battery-mobile participants never
    run heavy task kinds** — not even as a last resort".

    **No single kind can exercise it**, which is why this reads across rows:
    ``index`` is never nest-run (the content index is built at client capability
    positions — § The assignment picker), and the other two are never
    client-run. So the sentence's clauses are witnessed where each one is real,
    on one page, under no pin at all:

    1. **"it waits instead"** — ``backup-upload`` has no eligible participant,
       and reads *Waiting* while a plugged-in desktop is live and eligible for a
       different kind. The policy queues rather than pressing an ineligible
       participant into service.
    2. **"on your always-on nest when it can"** — an ``AlwaysOnNest``
       participant claims that same lease and the row names it. The stand-in's
       pubkey is this nest's own ``nest.info`` identity, byte-for-byte what
       ``delegation_runner::nest_self_ref`` claims with, so the row names the
       real home nest.
    3. **"otherwise on a plugged-in computer"** — no nest can run ``index``, so
       a plugged-in desktop is the next tier. Where this app builds the index
       (``RUNS``) the desktop is **this device** and the row says so; web and
       the mobiles never build, so the user's computer is a real tui seat of the
       same account and the row names *it*.
    4. **"never on a phone or tablet"** — a ``BatteryMobile`` participant claims
       the ``index`` lease, and the desktop **takes it straight back**. That is
       the live half of the rule: ``current_candidates`` gives a battery mobile
       no tier at all, so ``decide`` sees a fresh holder that is not a candidate
       and preempts it. The decision runs in a real runner, which is why the
       viewer columns get a real desktop seat rather than a stand-in heartbeat —
       a stand-in cannot decide anything. The heartbeat reply is the causal
       barrier: it carries the holder the nest recorded at that instant, so the
       takeover is asserted against a lease the phone genuinely held.

    ``real_conversations``: windows and macOS seat the ``index`` lease only when
    their conversations loop runs (see
    ``test_the_index_row_names_this_device_as_builder_of_record``); linux and tui
    seat it unconditionally, and the viewers never seat it at all.
    """
    from helpers import delegation_lease as lease

    seats = delegation_seats
    app, user = seats.observer, seats.user
    if not seats.observer_runs_index:
        seats.launch("tui", "desktop")

    td = app.task_delegation
    td.navigate()
    assert td.is_page_visible(), (
        f"task-delegation sub-page did not render; error={app.error_text()!r}"
    )
    backup_row = td.row_of(KIND_BACKUP_UPLOAD)
    index_row = td.row_of(KIND_INDEX)
    assert td.assignment_key(backup_row) == AUTOMATIC
    assert td.assignment_key(index_row) == AUTOMATIC

    # (1) It waits instead.
    runner = td.wait_for_runner(backup_row, RUNNER_WAITING)
    assert runner == RUNNER_WAITING, (
        f"no participant can run {KIND_BACKUP_UPLOAD!r} here — it is nest-run "
        f"and this nest holds no grant for it — so the row must say it is "
        f"waiting rather than name a participant. It reads {runner!r}."
    )
    # (2) …on your always-on nest when it can.
    nest_ref = lease.nest_holder(nest_instance)
    lease.heartbeat(
        nest_instance, user, "backup-upload",
        holder=nest_ref, holder_class="AlwaysOnNest",
    )
    # `participant_name` renders a nest ref as `short_id(hex(actor_pubkey))` —
    # always the 12-hex elision, never a roster label (the roster is keyed by
    # device id), so the expected text is exact rather than a substring.
    nest_hex = bytes(nest_ref["Nest"]["actor_pubkey"]).hex()
    runner = td.wait_for_runner_where(backup_row, lambda t: nest_hex[:12] in t)
    assert nest_hex[:12] in runner, (
        f"an always-on nest holds the {KIND_BACKUP_UPLOAD!r} lease, so the row "
        f"must name it ({nest_hex[:12]}…); it reads {runner!r}. Nest leases are "
        f"what the tier-1 half of the policy order is *for*. "
        f"nest view: {lease.observe(nest_instance, user, 'backup-upload')!r}"
    )

    # (3) …otherwise on a plugged-in computer.
    if seats.observer_runs_index:
        def on_the_desktop(t: str) -> bool:
            return t == RUNNER_THIS_DEVICE
        which = "this plugged-in desktop"
    else:
        def on_the_desktop(t: str) -> bool:
            return t.startswith("Running on ") and t != RUNNER_THIS_DEVICE
        which = "the user's plugged-in computer (a tui seat of this account)"
    runner = td.wait_for_runner_where(index_row, on_the_desktop)
    assert on_the_desktop(runner), (
        f"no nest runs {KIND_INDEX!r}, so tier 2 — {which} — is the policy's "
        f"pick and the row must name it; it reads {runner!r}. "
        f"nest view: {lease.observe(nest_instance, user, 'index')!r} "
        f"error={app.error_text()!r}"
    )

    # (4) …and never on a phone or tablet.
    phone = secrets.token_hex(32)
    recorded = lease.heartbeat(
        nest_instance, user, "index",
        holder=lease.device_holder(phone), holder_class="BatteryMobile",
    )
    assert phone in repr(recorded), (
        f"the phone's claim was not recorded, so nothing was taken back and the "
        f"assertion below would pass without a takeover: {recorded!r}"
    )
    runner = td.wait_for_runner_where(
        index_row, lambda t: on_the_desktop(t) and phone[:12] not in t
    )
    assert on_the_desktop(runner) and phone[:12] not in runner, (
        f"a battery mobile claimed the {KIND_INDEX!r} lease and still holds it: "
        f"the row reads {runner!r}. A phone is never a candidate for a heavy "
        f"task kind — not even as a last resort — so an eligible desktop must "
        f"preempt it rather than leave the work parked there. "
        f"nest view: {lease.observe(nest_instance, user, 'index')!r}"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("task-delegation")
def test_a_kind_whose_runner_went_away_says_waiting_not_its_name(
    logged_in_app, nest_instance, test_user
):
    """**Outcome 8** — *a job with no live device to run it says it is waiting,
    instead of naming a device that is not working*.

    ``participants.md`` § Coordination primitive → *The ``index`` kind under the
    lease*: making the heartbeat attachment-lifetime is what keeps
    ``RunnerStatus`` truthful, so that "``Waiting`` returns to meaning what it
    says — *no builder device is live right now*". The failure this forbids is a
    row that keeps naming a device which stopped days ago, because the user then
    believes work is happening that is not.

    ``test_the_index_row_names_this_device_as_builder_of_record`` asserts only
    the opposite face (a live builder IS named), and nothing in the suite drove
    a runner away — so a page that never re-read the lease's age would pass
    every existing test.

    The departure is driven through the nest's age seam rather than by waiting
    out ``LEASE_STALE_MS`` (90 s), which convention 14 calls defunct on sight:
    ``age_past_stale`` back-dates the recorded heartbeat and **asserts the
    resulting age**, so the boundary is crossed as a step rather than raced.

    ``backup-upload`` is the kind, deliberately: it is ``client_runnable =
    false``, so no client contends for it and no app can take over when the
    stand-in goes — which is what makes *Waiting* the only correct answer rather
    than one of two, on every app alike.
    """
    from helpers import delegation_lease as lease

    stand_in = secrets.token_hex(32)
    lease.heartbeat(
        nest_instance, test_user, "backup-upload",
        holder=lease.device_holder(stand_in), holder_class="PluggedInDesktop",
    )

    td = logged_in_app.task_delegation
    td.navigate()
    assert td.is_page_visible(), (
        f"task-delegation sub-page did not render; error={logged_in_app.error_text()!r}"
    )
    row = td.row_of(KIND_BACKUP_UPLOAD)

    # An unrostered device renders by the shared `short_id` 12-hex elision.
    runner = td.wait_for_runner_where(row, lambda t: stand_in[:12] in t)
    assert stand_in[:12] in runner, (
        f"a live device holds the {KIND_BACKUP_UPLOAD!r} lease, so the row must "
        f"name it ({stand_in[:12]}…) before this journey can say anything about "
        f"it going away; it reads {runner!r}. "
        f"nest view: {lease.observe(nest_instance, test_user, 'backup-upload')!r}"
    )

    lease.age_past_stale(nest_instance, test_user, "backup-upload")

    runner = td.wait_for_runner(row, RUNNER_WAITING)
    assert runner == RUNNER_WAITING, (
        f"the device holding {KIND_BACKUP_UPLOAD!r} stopped heartbeating, so the "
        f"row must say the job is waiting rather than keep naming it; it reads "
        f"{runner!r}. A row that names a device which is not working tells the "
        f"user work is happening that is not. "
        f"nest view: {lease.observe(nest_instance, test_user, 'backup-upload')!r}"
    )


def _pin_to_itself(seat, label: str) -> int:
    """``seat`` pins ``index`` to itself through its own picker (convention 8);
    returns the row. Only a seat in ``RUNS`` is ever asked to."""
    td = seat.task_delegation
    td.navigate()
    assert td.is_page_visible(), (
        f"{label}'s task-delegation page did not render; error={seat.error_text()!r}"
    )
    row = td.row_of(KIND_INDEX)
    assert td.assignment_key(row) == AUTOMATIC
    td.set_assignment(row, "this-device")
    assert td.wait_for_assignment(row, THIS_DEVICE), (
        f"{label}'s pin did not take; got {td.assignment_key(row)!r} "
        f"error={seat.error_text()!r}"
    )
    return row


def _wait_for_assignment_reload(seat, accept, timeout: float = 60.0) -> tuple[int | None, str]:
    """Re-open ``seat``'s page until its ``index`` assignment satisfies
    ``accept``; returns ``(row, key)`` as last read. Each visit re-runs
    ``TaskDelegationView::load()``, so what is read is the synced record, not a
    stale frame.

    No pump poke, deliberately: a seat reads assignments off its account plane
    (``fauna.state.delegation``), and an assignment made on another seat reaches
    it through the runtime's own push arm at nudge latency
    (``account-client-lifecycle.md`` § The pump, wake source (1)). The 60 s budget is well inside the 300 s
    backstop, so a red here means the nudge chain is dead, not merely slow."""
    td = seat.task_delegation
    row, key = None, ""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        td.navigate()
        if td.is_page_visible():
            row = td.row_of(KIND_INDEX)
            key = td.assignment_key(row)
            if accept(key):
                break
        time.sleep(0.5)
    return row, key


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("task-delegation")
def test_an_assignment_made_on_another_device_shows_here_and_clears_here(delegation_seats):
    """**Outcome 7** — *assignments are the same on every device you own, and one
    made on another device shows here and can be cleared here*.

    ``participants.md`` § Coordination primitive: "The assignment record lives in
    the account-state plane kind ``fauna.state.delegation`` (client-authoritative,
    sealed + synced across the user's devices …) — so every one of the user's
    clients sees the same assignments, and the surface is client-revocable per
    the product invariants." § The assignment picker adds the corollary that makes it
    escapable: "an assignment the user already holds is always **rendered** (so
    a pin made from another client — or a stale one predating this rule — is
    visible and escapable), even where it would not be *offered*".

    ``test_pinning_the_kind_this_app_runs_to_this_device_persists`` proves the
    pin round-trips through the nest from ONE device; this is the pairwise
    fact, and the ``PinOption::Other`` arm — the whole rendering path for a
    foreign pin — is reachable by no other test.

    Seat B is the user's other device: a second launch of this app where this
    app can make an index pin, a tui seat where it cannot (web and the mobiles
    never offer one — :class:`_Seats`). Seat A, the app under test, is the one
    that must show the foreign pin and clear it — on every column.
    """
    seats = delegation_seats
    seat_a = seats.observer
    seat_b, _stop_b = seats.launch(seats.pinning_app, "b")

    _pin_to_itself(seat_b, "seat B")

    # A sees B's assignment — as a pin to SOMEONE ELSE, which is the distinction
    # the shared `delegation_rows` draws and the only thing that makes the row
    # honest: rendering it as Automatic would hide a choice the user made, and
    # rendering it as This device would claim a pin to the wrong participant.
    row_a, key = _wait_for_assignment_reload(seat_a, lambda k: k not in (AUTOMATIC, ""))
    assert row_a is not None and key not in (AUTOMATIC, ""), (
        f"seat A still reads {key!r} for {KIND_INDEX!r}: an assignment made on "
        f"another of the user's devices never reached this one, so the two "
        f"devices do not agree on what is assigned where. "
        f"error={seat_a.error_text()!r}"
    )
    assert key != THIS_DEVICE, (
        f"seat A renders B's pin as a pin to ITSELF ({key!r}) — the row would "
        f"tell the user this device is assigned the job when another one is"
    )
    td_a = seat_a.task_delegation
    offered_a = _offered(td_a, row_a, seat_a)
    assert key in offered_a, (
        f"seat A's picker paints {offered_a!r}, which does not include the pin "
        f"it is currently showing ({key!r}) — a foreign assignment must stay "
        f"selectable-away rather than be a value the user cannot escape"
    )

    # …and can be cleared here. The revocation is a real pick on seat A.
    td_a.set_assignment(row_a, "automatic")
    assert td_a.wait_for_assignment(row_a, AUTOMATIC), (
        f"clearing B's pin from seat A did not take; got "
        f"{td_a.assignment_key(row_a)!r} error={seat_a.error_text()!r}"
    )

    # The clear is the same record on both devices, so B sees it too.
    _, key_b = _wait_for_assignment_reload(seat_b, lambda k: k == AUTOMATIC)
    assert key_b == AUTOMATIC, (
        f"seat B still reads {key_b!r}: the pin cleared on seat A did not reach "
        f"the device it was made on, so the user cannot revoke an assignment "
        f"from whichever device they happen to be holding. "
        f"error={seat_b.error_text()!r}"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.real_conversations
@pytest.mark.feature("task-delegation")
def test_a_kind_pinned_to_another_device_waits_for_it_rather_than_moving(
    delegation_seats, nest_instance
):
    """**Outcome 9** — *a job you assign to one device waits for that device
    rather than moving to another*.

    ``participants.md`` § The assignment picker: "A pin collapses the candidate
    set to exactly the pinned participant, which runs **iff currently
    eligible** — so pinning a participant that can *never* be eligible makes the
    task kind wait **forever**." That is deliberately NOT the takeover outcome
    (outcome 4, the nest's LWW arbitration): a pin is the user overriding the
    policy, and an override another device quietly reclaims is not an override.

    The journey is the whole promise end to end. Seat B pins ``index`` to itself;
    a competitor — an equally eligible plugged-in desktop that ships the same
    builder, and which may well have been holding the lease a moment earlier —
    stands down. Then B goes away, and the competitor **stays** standing down:
    the row reads *Waiting* rather than the competitor's own name.

    Who plays which part follows :class:`_Seats`. Where this app builds the
    index it IS the competitor, and B is a second launch of it. Web and the
    mobiles cannot compete (they never build), so B and the competitor are two
    tui seats of the same account — the user's two computers — and the app
    under test is the device the user is holding while it happens: it must name
    B while B runs and read *Waiting* after, never the competitor.

    The departure is the nest's age seam, not a 90 s wall-clock wait
    (convention 14). B's driver is torn down first so nothing re-heartbeats
    behind the assertion — a live B would renew on its next tick and the row
    would leave *Waiting* under the poll.

    ⚠ **This is the outcome whose witness hid a missing implementation.** Until
    it landed, ``index_lease.rs`` read the ``fauna.state.delegation`` pins exactly
    once per seat, at start: a pin made afterwards reached the Task-delegation
    *page* (which re-reads the pins on every visit) but never the running lease loop,
    so the competitor kept heartbeating a kind the user had assigned to B and
    the row named the wrong device until the app was restarted. The refresh
    (``index_lease::refresh_pins``) is what makes the middle leg below true, and
    this test is what keeps it true.

    ``real_conversations``: windows and macOS seat the ``index`` lease only when
    their conversations loop runs, and both B and the competitor must seat it.
    """
    from helpers import delegation_lease as lease

    seats = delegation_seats
    user = seats.user
    viewer = seats.observer
    seat_b, stop_b = seats.launch(seats.pinning_app, "b")
    if seats.observer_runs_index:
        competitor = viewer
    else:
        competitor, _stop_c = seats.launch("tui", "c")

    _pin_to_itself(seat_b, "seat B")

    # Everyone but B names somebody else. On the competitor this is it standing
    # down: the pinned participant is B, so it is not a candidate at all and
    # stops contending. The budget covers the pin refresh, which runs on the
    # lease loop's own HEARTBEAT_PERIOD_MS cadence (a periodic read, not a
    # push) — a deadline poll on rendered state, never a settle sleep.
    watchers = [("the competitor", competitor)]
    if competitor is not viewer:
        watchers.append(("the app under test", viewer))
    rows = {}
    for label, seat in watchers:
        td = seat.task_delegation
        td.navigate()
        assert td.is_page_visible(), (
            f"{label}'s task-delegation page did not render; error={seat.error_text()!r}"
        )
        rows[label] = td.row_of(KIND_INDEX)
        runner = td.wait_for_runner_where(
            rows[label],
            lambda t: t.startswith("Running on ") and t != RUNNER_THIS_DEVICE,
            timeout=120.0,
        )
        assert runner.startswith("Running on ") and runner != RUNNER_THIS_DEVICE, (
            f"{label}'s {KIND_INDEX!r} row reads {runner!r}. The user assigned "
            f"the job to their other device, so the row must name that device — "
            f"a pin another equally-eligible desktop reclaims is not an override. "
            f"nest view: {lease.observe(nest_instance, user, 'index')!r}"
        )

    # B goes away, and its lease goes with it.
    stop_b()
    lease.age_past_stale(nest_instance, user, "index")

    # …and the job waits for B rather than moving to the competitor. This is the
    # half that distinguishes the outcome from a takeover: the lease is free and
    # the competitor is eligible and capable, and it must still not take it.
    for label, seat in watchers:
        runner = seat.task_delegation.wait_for_runner(
            rows[label], RUNNER_WAITING, timeout=120.0
        )
        assert runner == RUNNER_WAITING, (
            f"{label}'s {KIND_INDEX!r} row reads {runner!r} after the pinned "
            f"device went away. The job is assigned to B, so it waits for B — "
            f"moving it would silently undo the user's assignment, which is "
            f"exactly what pinning exists to prevent. "
            f"nest view: {lease.observe(nest_instance, user, 'index')!r}"
        )
