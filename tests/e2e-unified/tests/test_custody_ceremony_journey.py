"""Tier_3 two-account custody-ceremony journey through the tui UI (T16, W8 (account-data-plane.md § Workstreams)).

The covering e2e for the custody facet: account A (the OWNER) asks a
friend to hold sealed copies via the Devices-page mint flow; account B (the
HOST) consents on ITS OWN app's consent card; the ceremony completes over the
real nest + real MLS channel (offer → accept → mint → deliver); B's account
pump meters the custody and mints an A7 receipt that reaches A's custodian
row; B's budget (metered against the cap picked at accept, then changed on
its own row), stop and revoke drive through the UI; and a second journey has
the recipient decline an offer instead. Every UI mutation is a driver
gesture (convention 8); every wait is a poked-cycle deadline poll, never a
cadence (convention 14 — ``poke_receive_cycle`` for channel delivery,
``poke_account_pump`` for the pump).

What this catches that nothing else can: the rust tier_3 rigs
(``conformance_custody_ceremony_client.rs``) prove the ceremony and receipt
legs over the real wire but CONSTRUCT their payloads — they cannot see an app
wiring the wrong identity into a gesture. This journey found exactly that on
its first authoring pass (2026-08-17): the tui accept bound the roster's
random ``device.db`` id instead of the device principal, a custody that could
never pull nor attest.

Spec: docs/goal/ui/devices.md § Custody facet; docs/goal/ui/nests.md § Trust
facet — custody rows; docs/goal/architecture/account-data-plane.md § Replica
posture → The custody grant + ceremony / Custody policy.
"""

from __future__ import annotations

import time

import pytest

from helpers import enrollment
from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S
from helpers.waiting import (
    account_pump_cycles,
    await_device_removal_ready,
    await_pump_cycle_after,
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_account_pump,
    poke_receive_cycle,
    wait_until,
)
from i18n.strings import S
from tests.api import conv_api

pytestmark = pytest.mark.tier_3


# The R14 (account-data-plane.md § The ratified decisions) escrow-holder trust this journey needs is seeded by DEFAULT for every
# app launch now — `conftest._apply_r14_trust_env` puts
# `FAUNA_E2E_TRUST_NEST_IDENTITY` (nest.info's `nest_id`) into
# `config["environment"]`, which is where the account runtime reads it at
# assembly. This test carried the seed as its own `seed_nest_escrow_trust`
# fixture when it was the plane's FIRST app-level consumer (2026-08-17); the
# fixture is gone because the posture is the default, and this journey is now
# one of the tests that would go red if the default ever regressed — a
# generation-sealed write (the custody registry rows) through the seeded trust.
# Contract: `e2e-automation-surface-gating.md` § The e2e trust seed.

# Generous per-leg budget for a UI state that only needs local folds after a
# poked cycle — far above any non-pathological delay (convention 14: named
# budgets, deadline polls).
_UI_LEG_S = 30.0

# The poked pump pass runs the WHOLE pump — enrollment, planes, peer leg, and
# the custody legs, whose dial pass may pay full transport timeouts against
# an owner device that is not listening (alice's app holds her own engine
# lock; her peer-leg listener may not be up in this rig). First-pass ceiling,
# far above any non-pathological delay; the pass COMPLETING is the barrier.
_PUMP_PASS_S = 300.0


def _custody_log_tail(app) -> str:
    """The custody/pump lines from one seat's app stderr — the failure
    narrative when a ceremony or pump leg refuses (a tip that never resolved,
    a registry write or hosting register left owed, a dial that ate the
    pass)."""
    try:
        text = app.driver.app_stderr_text() or ""
    except Exception as e:  # noqa: BLE001 — diagnostics must not mask the assert
        return f"(app stderr unreadable: {e})"
    wanted = ("account pump", "account_pump_now", "custody", "generation", "tip")
    lines = [ln for ln in text.splitlines() if any(w in ln for w in wanted)]
    # A poked pass logs its whole PumpReport on one line, so a plain tail is
    # forty copies of the same report and hides the one ceremony line that
    # says where the receipt stopped (a mint, a post, an ingest refusal).
    # Keep every other line, and only the LAST full report.
    reports = [ln for ln in lines if "pass complete" in ln]
    narrative = [ln for ln in lines if "pass complete" not in ln]
    kept = narrative[-60:] + reports[-1:]
    return "\n".join(kept) or "(no pump/custody lines in the app log)"



@pytest.fixture
def custody_pump_held(domainless_nest):
    """Hold ``domainless_nest``'s periodic custody-hosting loop for one test
    (the ``custody_hosting/hold`` test hook), releasing it at teardown.

    The module-scoped nest outlives every app param, and its pump ticks at
    boot and every 15 minutes after — so on a later param a scheduled pass can
    land between the accept and the no-confirmation-yet read, and the poke leg
    then finds the receipt already deposited. Held, the ONLY passes are the
    test's own ``run-now`` pokes (convention 14: state, never a race)."""
    import requests

    hook = f"{domainless_nest['url']}/api/v1/test/custody_hosting/hold"
    resp = requests.post(hook, json={"held": True}, timeout=30)
    assert resp.status_code == 200, (
        f"custody_hosting hold returned {resp.status_code}: {resp.text}"
    )
    yield
    requests.post(hook, json={"held": False}, timeout=30)


def _receive_once(app) -> None:
    """One poked receive cycle, completion-anchored."""
    cycles = conv_receive_cycles(app.driver)
    started = cycles[0] if cycles else None
    poke_receive_cycle(app.driver)
    await_receive_cycle_after(
        app.driver, started, budget_s=RECEIVE_CYCLE_S, what="the ceremony payload"
    )


def _open_carrier(alice, bob_app, bob, port) -> None:
    """Open the ceremony's carrier: a real 1:1 MLS conversation alice starts
    with bob (the mint's host list derives from 2-member conv channels), bound
    on bob's side before any ceremony payload rides it. Bob's key-package pool
    must SETTLE first — a >=1 read races the login top-up (the drain test's
    quiescence gate)."""
    quiet_s = 3.0
    deadline = time.time() + MLS_HANDSHAKE_S
    last, stable_since, settled = None, None, False
    while time.time() < deadline:
        count = conv_api.keypackage_count(port, bob, bob["actor_id_hex"])
        now = time.time()
        if count != last:
            last, stable_since = count, now
        elif count >= 1 and now - stable_since >= quiet_s:
            settled = True
            break
        time.sleep(0.5)
    assert settled, f"bob's key-package pool never settled (last {last!r})"

    alice.conversations.real_resolve_send_new(bob["actor_id_hex"], "custody carrier")
    # Bob binds the channel (Welcome + history) — his sink must hold the
    # channel before the ceremony's payloads arrive on it.
    _receive_once(bob_app)


def _poll_ui(app, predicate, *, what: str, receive: bool = False, budget_s: float = _UI_LEG_S):
    """Deadline-poll a UI predicate, re-hydrating the Devices page each try
    (the facet folds on the nav edge) and optionally poking a receive cycle
    first (the arrival may land on either side of one sweep)."""
    deadline = time.time() + budget_s
    while time.time() < deadline:
        if receive:
            _receive_once(app)
        app.backups.navigate_devices()
        if predicate():
            return
        time.sleep(0.5)
    raise AssertionError(f"never observed: {what}")


# `real_conversations` on both tests: `real_faunamls_app` activates the real
# engine through a LAUNCH-TIME gate on windows/macOS/iOS/android, which only this
# marker sets. Without it windows errors at fixture setup (a 60 s readiness
# timeout) instead of reaching `require_supported()`'s declared skip.
@pytest.mark.real_conversations
@pytest.mark.feature("backup-destinations-and-restore")
@pytest.mark.feature("devices")
def test_custody_ceremony_two_accounts(
    real_faunamls_app, second_real_faunamls_app, nest_instance
):
    alice = real_faunamls_app
    bob_app, bob = second_real_faunamls_app
    alice.custody.require_supported()
    port = nest_instance["port"]

    # ── The carrier: a real 1:1 MLS conversation (the mint host list derives
    # from 2-member conv channels).
    _open_carrier(alice, bob_app, bob, port)

    # ── Offer initiation (owner side, through the UI). The sole 1:1 thread is
    # the sole mint candidate; a wrong label fails loudly listing the painted
    # options (the select refuses unoffered values, convention 11).
    alice.backups.navigate_devices()
    alice.custody.mint_offer(bob["actor_id_hex"])

    # The confirm records the offer and the drive posts it; alice's own row
    # family shows the pending ceremony at once (fold of her own config).
    _poll_ui(
        alice,
        lambda: alice.custody.holder_count() >= 1,
        what="alice's pending custodian row after the mint",
    )

    # ── Consent (host side, through the UI): the offer arrives on bob's poked
    # receive cycle; the card renders the REQUIRED floor copy before accept.
    _poll_ui(
        bob_app,
        lambda: bob_app.custody.offer_count() >= 1,
        what="the consent card on bob's Devices page",
        receive=True,
    )
    assert bob_app.custody.offer_floor_note(0) == S.devices.custody_offer_floor, (
        "the consent card MUST state the custody floor before accept "
        "(devices.md § Custody facet piece 3 — REQUIRED copy)"
    )
    bob_app.custody.accept_offer(0)

    # The accept posts on the post-accept drive; alice ingests it, her drive
    # mints + delivers, and her row leaves pending (revoke becomes enabled —
    # the render gates it on a minted ceremony).
    _poll_ui(
        alice,
        lambda: alice.custody.holder_count() >= 1
        and alice.driver.is_enabled("custody-holder-revoke-button"),
        what="alice's custodian row bound + minted (revoke enabled)",
        receive=True,
    )
    # Three-state honesty: no receipt yet is its OWN state with its own words.
    assert alice.custody.holder_receipt_status(0) == S.devices.custody_receipt_none

    # ── Bob ingests the deliver (witness held; his drive writes the held
    # registry row) and his held-for-others row renders.
    _poll_ui(
        bob_app,
        lambda: bob_app.custody.held_count() >= 1,
        what="bob's held-for-others row after the deliver",
        receive=True,
    )
    assert bob["actor_id_hex"] not in bob_app.custody.held_owner(0), (
        "the held row names the OWNER (alice), never the host itself"
    )

    # ── The A7 receipt: bob's account pump meters the custody and mints
    # (never-attested ⇒ due), and the poke's chained drive posts it over the
    # same channel. The pump barrier is the cycle counters, never a cadence.
    pump = account_pump_cycles(bob_app.driver)
    assert pump is not None, (
        "bob's app publishes no account_pump_cycles — the pump barrier is "
        "unimplemented on this app (fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY)"
    )
    poke_account_pump(bob_app.driver)
    try:
        await_pump_cycle_after(
            bob_app.driver, pump[0], budget_s=_PUMP_PASS_S, what="the receipt mint pass"
        )
    except AssertionError as e:
        raise AssertionError(f"{e}\n--- bob's pump log ---\n{_custody_log_tail(bob_app)}") from e

    # Alice reads it: her poked receive ingests the receipt and the row flips
    # to the fresh state — "last confirmed ‹time›", the success line.
    # Each try ALSO pokes bob's pump: the held registry row lands on one drive
    # retry (the first-need generation mint fires on that very write), the
    # receipt mints on the next pass over the landed row, and the poke's
    # chained drive posts it — convergence anchored on state, never a count
    # of passes.
    last_pump_poke = [0.0]

    def _fresh() -> bool:
        # A full pump pass is expensive — space the pokes; the poll's own
        # receive+re-nav cadence stays at the loop's half-second.
        if time.time() - last_pump_poke[0] > 10.0:
            poke_account_pump(bob_app.driver)
            last_pump_poke[0] = time.time()
        status = alice.custody.holder_receipt_status(0)
        return status.startswith("Last confirmed")

    try:
        _poll_ui(
            alice,
            _fresh,
            what="alice's row reading the custodian's verified receipt",
            receive=True,
            budget_s=240.0,
        )
    except AssertionError as e:
        # Both seats: the receipt is minted and posted by bob but ingested
        # and verified by alice, and either half can be where it stopped.
        raise AssertionError(
            f"{e} (alice's row reads {alice.custody.holder_receipt_status(0)!r})"
            f"\n--- bob's pump log ---\n{_custody_log_tail(bob_app)}"
            f"\n--- alice's custody log ---\n{_custody_log_tail(alice)}"
        ) from e

    # ── The budget (host side, piece 3): what bob holds is metered against the
    # cap picked at accept, and he changes it here. The accept picks the
    # shared default (`custody_ceremony::DEFAULT_RETAINED_BYTES_CAP`, 8 GiB);
    # bob's own receipt — minted above — meters against exactly that cap.
    accepted_cap = S.size.gb(value="8")
    assert bob_app.custody.held_budget(0) == accepted_cap, (
        "the budget input must seed from the cap the accept picked: "
        f"{bob_app.driver.diagnose('custody-held-budget-input')}"
    )
    _poll_ui(
        bob_app,
        lambda: bob_app.custody.held_bytes(0).startswith("Holding ")
        and f" of {accepted_cap}" in bob_app.custody.held_bytes(0),
        what=f"bob's held row metering his receipt against the accepted {accepted_cap}",
    )
    # Change it through the input (convention 8). The re-seeded text is the
    # persisted registry cap formatted back — "3 GB", not the typed
    # "3072 MB" — so a match proves the write landed, not that the draft stuck.
    # (The next receipt re-attests against the new cap on the check-in
    # cadence or at the first eviction — `custody_receipt::receipt_due` —
    # which no test waits out, convention 14.)
    bob_app.custody.set_held_budget("3072 MB")
    changed_cap = S.size.gb(value="3")
    try:
        _poll_ui(
            bob_app,
            lambda: bob_app.custody.held_budget(0) == changed_cap,
            what=f"bob's budget input re-seeding the changed cap {changed_cap}",
        )
    except AssertionError as e:
        error = (
            bob_app.driver.get_text("error-message")
            if bob_app.driver.is_visible("error-message")
            else ""
        )
        raise AssertionError(
            f"{e} (input reads {bob_app.custody.held_budget(0)!r}; error-message: {error!r})"
        ) from e

    # ── A device of bob's that holds no keys (piece 1), and a removal the app
    # cannot verify (devices.md § Errors & edge cases). Bob's generation tip
    # resolves now — the held registry row above was a generation-sealed
    # write — so the keyless-posture derivation has a tip to read. The fixture
    # plants a granted row whose key never joined bob's fleet: no wrap at the
    # tip (marked relay-only, a fact with no control) and no verified member
    # (its removal is refused as unverifiable and deletes nothing).
    relay_label = "relay-only-kiosk"
    relay_row, _relay_principal = enrollment.register_granted_device(
        nest_instance["url"], bob, relay_label
    )

    def _relay_card() -> int | None:
        for i in range(bob_app.driver.count("device-card")):
            if bob_app.backups.device_name(i) == relay_label:
                return i
        return None

    def _relay_marked() -> bool:
        i = _relay_card()
        return i is not None and bob_app.driver.is_visible_scrolled(
            "device-keyless-posture-badge", scope=f"device-card[{i}]"
        )

    _poll_ui(bob_app, _relay_marked, what="bob's keyless device row marked relay-only")
    assert (
        bob_app.driver.get_text("device-keyless-posture-badge", scope=f"device-card[{_relay_card()}]")
        == S.devices.keyless_posture_badge
    )
    # The marker is per device, never "the account": bob's own device holds
    # the keys it minted, so its row carries no marker.
    own = next(
        (
            i
            for i in range(bob_app.driver.count("device-card"))
            if bob_app.driver.is_visible_scrolled("device-this-mark-badge", scope=f"device-card[{i}]")
        ),
        None,
    )
    assert own is not None, bob_app.driver.diagnose("device-this-mark-badge")
    assert not bob_app.driver.is_visible_scrolled(
        "device-keyless-posture-badge", scope=f"device-card[{own}]"
    ), "the device that minted bob's generation holds its keys — no relay-only marker"

    await_device_removal_ready(bob_app.driver)
    bob_app.driver.click("device-remove-button", index=_relay_card())
    # No re-navigation while waiting: a hydrate refresh clears the page error.
    refusal: dict = {}

    def _refused() -> bool:
        refusal["error"] = (
            bob_app.driver.get_text("error-message")
            if bob_app.driver.is_visible("error-message")
            else ""
        )
        return refusal["error"] == S.devices.error_remove_unverified_device

    wait_until(
        _refused,
        _UI_LEG_S,
        interval=0.5,
        diagnose=lambda: (
            "removing a device no verified fleet member accounts for was never refused "
            f"as unverifiable (last error-message: {refusal.get('error')!r})"
        ),
    )
    assert relay_row in enrollment.roster(nest_instance["url"], bob), (
        "a refused removal must not delete the row on the nest"
    )
    assert _relay_card() is not None, "the refused row must still be listed"

    # ── Stop (host side): always available; the row then honestly shows the
    # hold has ended (the button disables — nothing further to stop).
    bob_app.custody.stop_holding(0)
    _poll_ui(
        bob_app,
        lambda: bob_app.custody.held_count() >= 1
        and not bob_app.driver.is_enabled("custody-held-stop-button"),
        what="bob's held row showing the hold ended",
    )

    # ── Revoke (owner side): the custody leaves alice's Now rows — History's
    # business thereafter (the honest bound stays stated beside the control).
    alice.backups.navigate_devices()
    alice.custody.revoke_holder(0)
    _poll_ui(
        alice,
        lambda: alice.custody.holder_count() == 0,
        what="alice's custodian row leaving the Now list after revoke",
    )


@pytest.mark.real_conversations
@pytest.mark.feature("devices")
def test_a_declined_custody_offer_goes_away(
    real_faunamls_app, second_real_faunamls_app, nest_instance, test_user
):
    """An offer to hold copies that you do not want is declined on your Devices
    page and goes away — no card, no held row, no error — and stays gone across
    a further receive cycle and re-fold (``devices.md`` § Where logic lives —
    decline is one of the five custody gestures, ``run_custody_act``'s
    ``Decline`` arm recording it in the decliner's own config so every later
    fold leaves it out).

    ⚠ **The roles are the reverse of the ceremony journey on purpose:** the
    fresh second account (bob) OFFERS and the session's shared account (alice,
    ``test_user``) declines. A decline is recorded on the decliner only, so its
    owner's custodian row stays pending; an offer minted BY the shared account
    would leave that pending row on it for every later custody journey in the
    session (the ceremony journey's revoke leg counted it). Declining leaves
    the shared account only a hidden record."""
    alice = real_faunamls_app
    bob_app, bob = second_real_faunamls_app
    alice.custody.require_supported()

    _open_carrier(alice, bob_app, bob, nest_instance["port"])
    bob_app.backups.navigate_devices()
    # Bob's thread with alice is labelled by her short id
    # (`fauna_core::format::short_id`: 12 hex + `…`) — the joiner's side of
    # the carrier, where the opener's side shows the full hex. The select
    # refuses an unoffered value loudly, listing what it painted.
    bob_app.custody.mint_offer(test_user["actor_id_hex"][:12] + "…")

    _poll_ui(
        alice,
        lambda: alice.custody.offer_count() >= 1,
        what="the consent card on alice's Devices page",
        receive=True,
    )
    offers_before = alice.custody.offer_count()
    held_before = alice.custody.held_count()
    alice.custody.decline_offer(0)
    _poll_ui(
        alice,
        lambda: alice.custody.offer_count() == offers_before - 1,
        what="the declined offer leaving alice's Devices page",
    )

    # Gone for good, not for one paint: a further receive cycle and a fresh
    # nav-edge fold still leave it out, and nothing was accepted by accident.
    _receive_once(alice)
    alice.backups.navigate_devices()
    assert alice.custody.offer_count() == offers_before - 1, alice.driver.diagnose(
        "custody-offer-card"
    )
    assert alice.custody.held_count() == held_before, (
        "a declined offer must never become a held row: "
        f"{alice.driver.diagnose('custody-held-card')}"
    )
    assert not alice.driver.is_visible("error-message"), (
        f"the decline must not surface an error: {alice.driver.get_text('error-message')!r}"
    )


@pytest.mark.real_conversations
@pytest.mark.feature("backup-destinations-and-restore")
@pytest.mark.feature("nests-and-trust")
@pytest.mark.feature("admin-held-custody")
def test_custody_ceremony_nest_anchored(
    domainless_real_faunamls_app,
    domainless_second_real_faunamls_app,
    domainless_nest,
    custody_pump_held,
):
    """The custodian-NEST runtime end to end through both tui UIs (the
    success rig — the nest-custodian identity fact, item 6 complete): bob's
    consent card offers the target select (advertising offer + pinned nest
    identity), he picks "My nest" and accepts; his app deposits the
    custody-hosting row on his nest and takes NO further part; the NEST's
    pump — poked over the test hook, never cadence-waited — pulls alice's
    covered planes and deposits the A7 receipt at alice's nest custody door;
    alice's NESTS page renders the custodian nest with the three-state
    honesty, and the row never appears among her Devices holder cards (the
    render split: one custody never renders in both).

    The no-host-PROCESS pull mechanism itself is pinned by the Rust tier_3
    conformance (``conformance_custody_hosting_pump_client``: bob's client
    disconnects before the pass); here bob's app is idle after the deposit,
    and every pump pass is nest-side.

    Runs on ``domainless_nest``, never the shared ``nest_instance``: the
    pump dials alice's nest at the plaintext loopback URL this rig serves, and
    the shared nest's pinned ``fauna.test`` identity makes it a PUBLIC
    deployment, where the nest-scope dial policy refuses exactly that URL at
    the register door — no hosting row lands and every pump pass reports
    ``rows: 0`` (the first tui run, 2026-09-22). A private nest is the honest
    shape for a plaintext loopback owner URL.
    """
    import requests as _requests

    alice = domainless_real_faunamls_app
    bob_app, bob = domainless_second_real_faunamls_app
    nest = domainless_nest
    alice.custody.require_supported()
    alice.nest_trust.require_custody_rows_supported()
    port = nest["port"]

    # ── The carrier conversation.
    _open_carrier(alice, bob_app, bob, port)

    # ── Offer (owner side, through the UI). The production mint advertises
    # the nest binding and carries alice's nest URL — the select's two
    # offer-side preconditions.
    alice.backups.navigate_devices()
    alice.custody.mint_offer(bob["actor_id_hex"])
    _poll_ui(
        alice,
        lambda: alice.custody.holder_count() >= 1,
        what="alice's pending custodian row after the mint",
    )

    # ── Consent (host side): the card renders WITH the target select — the
    # host-side choice exists exactly here (advertising offer + bob's app
    # holds a pinned identity for his own nest from its first connect).
    _poll_ui(
        bob_app,
        lambda: bob_app.custody.offer_count() >= 1
        and bob_app.custody.offer_has_nest_choice(0),
        what="the consent card WITH the nest choice on bob's Devices page",
        receive=True,
    )
    bob_app.custody.accept_offer_on_nest(0)

    # ── Three-state honesty, state 1 (`nests.md` § Trust facet — custody
    # rows): the ceremony completes over the channel with NO pump pass — the
    # periodic loop is held (`custody_pump_held`) and every pass below is an
    # explicit poke — so once alice's Nests page lists the custodian nest, it
    # has confirmed nothing yet, and says so in its own words (never "fresh",
    # never an omitted row). alice is this test's own account
    # (`domainless_user`), so the row is this ceremony's, never an earlier
    # app param's already-confirmed custody.
    def _nests_row_listed() -> bool:
        _receive_once(alice)
        _receive_once(bob_app)
        alice.linked_nests.navigate()
        return alice.nest_trust.custody_count() >= 1

    deadline = time.time() + 240.0
    listed_pre_pump = False
    while time.time() < deadline:
        if _nests_row_listed():
            listed_pre_pump = True
            break
        time.sleep(0.5)
    assert listed_pre_pump, (
        "the custodian nest never reached alice's Nests page after the accept"
        f"\n--- alice's custody log ---\n{_custody_log_tail(alice)}"
    )
    assert alice.nest_trust.custody_receipt_status_text(0) == (
        S.devices.custody_receipt_none
    ), (
        "before any pump pass the custodian has confirmed nothing — the row must "
        f"read {S.devices.custody_receipt_none!r}, got "
        f"{alice.nest_trust.custody_receipt_status_text(0)!r}"
    )

    # ── The ceremony completes over the channel (accept → mint + deliver →
    # deposit), then the NEST takes over. Every pump pass is an explicit
    # nest-side poke; the tallies are latency-independent state. The
    # receive pokes carry the ceremony payloads both ways; once a pass
    # reports a completed pull AND a deposited receipt has been seen, the
    # nest half is done — bob's app takes no part in any pass.
    hook = f"{nest['url']}/api/v1/test/custody_hosting/run-now"
    receipt_seen = False
    pull_seen = False
    state = {}
    deadline = time.time() + 240.0
    while time.time() < deadline:
        _receive_once(alice)
        _receive_once(bob_app)
        resp = _requests.post(hook, timeout=30)
        assert resp.status_code == 200, (
            f"custody_hosting run-now returned {resp.status_code}: {resp.text}"
        )
        state = resp.json()
        pull_seen = pull_seen or state.get("pulled", 0) >= 1
        receipt_seen = receipt_seen or state.get("receipts_deposited", 0) >= 1
        if pull_seen and receipt_seen:
            break
        time.sleep(0.5)
    # `rows: 0` in the report means no hosting row ever reached the nest — a
    # break upstream of the pump (alice's mint/deliver, or bob's register
    # door refusing the deposit), so both seats' ceremony lines ride the
    # failure (convention 6).
    assert pull_seen and receipt_seen, (
        "the nest pump never completed a pull + receipt deposit; "
        f"last pass report: {state}"
        f"\n--- bob's custody log ---\n{_custody_log_tail(bob_app)}"
        f"\n--- alice's custody log ---\n{_custody_log_tail(alice)}"
    )

    # ── Owner render (the fold split + three-state honesty): the custodian
    # NEST appears on alice's Nests page reading fresh — "last confirmed
    # ‹time›" — and never among her Devices holder cards. Her nests hydrate
    # fetches the staged receipt itself (stage (c)'s owner-side leg rides
    # every facet refresh), so the poll is navigation alone.
    def _nests_row_fresh() -> bool:
        alice.linked_nests.navigate()
        if alice.driver.count("nest-trust-custody-item") < 1:
            return False
        status = alice.driver.get_text("nest-trust-custody-receipt-status", index=0)
        return status.startswith("Last confirmed")

    deadline = time.time() + 240.0
    ok = False
    while time.time() < deadline:
        _receive_once(alice)
        if _nests_row_fresh():
            ok = True
            break
        time.sleep(0.5)
    assert ok, "alice's Nests page never rendered the custodian nest fresh"

    # ── Three-state honesty, state 3: the custodian stops confirming. The
    # staleness window is a 48-hour Rust constant, so the trust facet's
    # RENDER clock moves past it (`trust_facet_advance_clock` — convention 14's
    # fake clock) while no further pump pass runs: the row reads stale —
    # weakened protection the owner sees — and is never dropped.
    alice.nest_trust.require_trust_clock_supported()
    alice.nest_trust.advance_trust_clock(3 * 24 * 60 * 60)
    try:
        stale_prefix = S.devices.custody_receipt_stale(when="\0").split("\0")[0]

        def _nests_row_stale() -> bool:
            alice.linked_nests.navigate()
            return (
                alice.nest_trust.custody_count() >= 1
                and alice.nest_trust.custody_receipt_status_text(0).startswith(
                    stale_prefix
                )
            )

        deadline = time.time() + _UI_LEG_S
        stale = False
        while time.time() < deadline:
            if _nests_row_stale():
                stale = True
                break
            time.sleep(0.5)
        assert stale, (
            "past the staleness window the custodian row must read stale, not "
            f"vanish; {alice.nest_trust.custody_count()} row(s), status "
            f"{alice.nest_trust.custody_receipt_status_text(0)!r}"
        )
        # … and still says how much the custodian holds (the receipt's
        # held-bytes line rides the row whatever its freshness).
        held = alice.driver.get_text("nest-trust-custody-held-bytes", index=0) or ""
        assert held.strip(), "the custodian row must say how much it holds"
    finally:
        alice.nest_trust.advance_trust_clock(0)

    # ── The escrow-holder role badge (`participants.md` § The participant
    # model): alice's account generation keys are escrowed at her home nest
    # (the default e2e trust seed names it — `conftest._apply_r14_trust_env`),
    # so the home row (first, `nests.md` § Layout) wears the badge; it is
    # derived from her recorded escrow receipts, never asserted by the nest.
    alice.linked_nests.navigate()
    assert alice.nest_trust.escrow_badge_text(0) == S.nests.escrow_holder_badge, (
        "alice's home nest holds her recovery escrow, so its row must wear the "
        f"escrow-holder badge; got {alice.nest_trust.escrow_badge_text(0)!r}"
    )

    # The render split: the same custody is NOT a Devices holder card.
    alice.backups.navigate_devices()
    assert alice.custody.holder_count() == 0, (
        "a nest-anchored custody must render on the Nests page ONLY — "
        "one custody never renders in both (the device-or-nest bullet, item 5)"
    )

    # ── The ADMIN half (`account-data-plane.md`
    # § Two-sided bounds). Everything above proved the row does its job. This
    # proves the nest's operator can SEE and UNDO it — the recoverability half
    # of *no client-causable unrecoverable nest state*.
    #
    # bob deposited this row through his own UI; `fauna.custody.hosting.list`
    # is host-scoped, so alice — and every other admin — could not see it at
    # all before the admin doors. The switch to the admin identity is the last
    # thing this test does: an identity switch is one-way in practice, and no
    # alice assertion follows it.
    admin_secret = nest["admin"]["signing_key"].encode().hex()
    alice.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": admin_secret,
            "device_id": "e2e-admin-device",
        },
        "nav": {"stack": [{"view": "admin"}]},
    })
    alice.driver.wait_for("admin-dashboard-heading", timeout=30.0)

    # An identity-only assertion cannot pin a session switch (`AuthSuccess`
    # fires before the WS-RPC is up, and a 403'd handshake retries forever,
    # reading as "empty, no error"). The registry read below IS the nest-backed
    # round trip that pins it: it returns bob's row, which only an authenticated
    # ADMIN connection can produce.
    _ADMIN_LEG_S = 60.0

    # What the page last showed — an unanswered read, an honestly empty one,
    # and an error are three different failures (convention 6).
    last_seen = {}

    def _registry_lists_bobs_row() -> bool:
        alice.admin.navigate_custody_hosting()
        last_seen.update(
            answered=alice.admin.hosting_registry_answered(),
            empty=alice.driver.count("admin-custody-hosting-empty") > 0,
            rows=alice.admin.hosting_row_count(),
            error=alice.driver.get_text("error-message")
            if alice.driver.count("error-message") > 0
            else "",
        )
        return last_seen["answered"] and last_seen["rows"] >= 1

    deadline = time.time() + _ADMIN_LEG_S
    listed = False
    while time.time() < deadline:
        if _registry_lists_bobs_row():
            listed = True
            break
        time.sleep(0.5)
    assert listed, (
        "the admin registry never listed the hosting row bob planted — the "
        "nest-wide list is the ONLY surface that can see another account's row; "
        f"the page last showed {last_seen}"
    )

    # The row is attributed to its DEPOSITING host and carries the address the
    # pump dials — the two facts an admin needs to hold someone responsible.
    assert alice.admin.hosting_row_host(0).startswith(bob["actor_id_hex"][:8]), (
        f"the row must name bob as the host, got {alice.admin.hosting_row_host(0)!r}"
    )
    assert alice.admin.hosting_row_url(0).startswith("http"), (
        "the row must render the owner-nest URL the pump dials, verbatim"
    )
    # The pump completed a pull above, so the metering and the receipt are real
    # rather than the never-ran placeholders.
    assert alice.admin.hosting_row_receipt(0) == S.admin.custody_hosting.receipt_fresh, (
        "a row the pump just attested must read fresh, got "
        f"{alice.admin.hosting_row_receipt(0)!r}"
    )

    # Cancel touches nothing — the confirm is a real gate, not decoration.
    alice.driver.click("admin-custody-hosting-remove-button", index=0)
    alice.driver.wait_for("admin-custody-hosting-remove-confirm-button", timeout=10.0)
    alice.admin.hosting_cancel_remove()
    assert not alice.admin.hosting_remove_armed(), "cancel must disarm the confirm"
    assert alice.admin.hosting_row_count() >= 1, "cancel must remove nothing"

    # …and the remove really drops it. This is the affordance that replaces
    # `sqlite3` on `nest.db` plus `rm -rf`.
    alice.admin.hosting_remove(0)

    def _row_gone() -> bool:
        alice.admin.navigate_custody_hosting()
        return alice.admin.hosting_registry_answered() and (
            alice.admin.hosting_row_count() == 0
        )

    deadline = time.time() + _ADMIN_LEG_S
    gone = False
    while time.time() < deadline:
        if _row_gone():
            gone = True
            break
        time.sleep(0.5)
    assert gone, "the admin remove never dropped the row from the registry"
