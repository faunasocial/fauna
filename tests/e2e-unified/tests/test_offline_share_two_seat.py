"""tier_3 — the co-present ceremony driven end to end through TWO app UIs.

`p2p.md` § Offline share initiation, contract point 1. Two people sitting next
to each other, two accounts, one machine, no nest arbitrating the ceremony —
convention 16's two-seat shape, and the success condition row 61 was written
against: **the recipient's folders page lists the shared set**.

Why this is a journey and not more Rust
---------------------------------------
The ceremony's mechanism is already proven where mechanisms belong: the
two-party walk over a real peer channel
(`fauna-client-capabilities/tests/group_ceremony_node.rs`), the plane's
persistence across a restart (`fauna-sync-engine/tests/group_plane_restart.rs`),
and the paint decisions in tier_1 truth tables. Every one of those is handed
its inputs. What none of them can answer is the question a user actually has:
**if two people do this through the UI, does the folder show up?**

Between the last Rust assertion and that question sit the parts only a running
app has — the seat binding when a panel opens, the offer arriving on a listener
nobody polled, the consent card appearing on a page that was already open, the
account runtime's write doors, and the store read the folders page paints from.
A break anywhere there is invisible to every test above and total to the user.

The two blocking waits, and why they are not sleeps
--------------------------------------------------
Each side waits on *the other person*: Begin holds until the recipient taps
Accept, and Accept holds until the deliver crosses. Both ops are declared
`outlives_click`, so neither click holds its HTTP reply hostage to a human —
and this file therefore asserts on **surfaces**, polling each to a named
generous budget (convention 14). Nothing here asserts that anything took a
particular amount of time.

⚠ What a green run does NOT prove: content transfer. v1's scope ends at
admission — the set's machinery is adopted and the scope lists — because the
group serve door still checks the folder family only (`account-data-plane.md`
§ Implementation status today: content transfer waits on group content-kind
sealing). Bytes are the sealing slice's journey, not this one's.
"""

import os
import socket
import time

import pytest

from common.nest import start_nest_in_place, stop_nest
from conftest import _apply_r14_trust_env, get_available_apps
from drivers import create_driver
from helpers.app_surface import skip_environment
from helpers.diagnostics import strip_ansi
from helpers.trust_seed_witness import await_a_tip_sealed_row
from i18n.strings import S

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui leads the affordance (`p2p.md` § Offline share initiation → build
    # order); linux joined 2026-08-20; macos joined 2026-08-25
    # . The six-app trickle-down inherits the shared
    # crates this drives, and each leg arrives with its own marker AND joins
    # `_SUPPORTED_APPS` below — a marker alone still silently drove tui
    # underneath.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.windows,
]

#: Apps with a landed offline-share UI leg. Grow this — and the app's own
#: marker above — together; a marker with no `_SUPPORTED_APPS` entry left
#: `ceremony_seats` hardcoded to tui for a year.
_SUPPORTED_APPS = ("tui", "linux", "macos", "windows")


def _offline_share_apps():
    available = get_available_apps()
    return [c for c in _SUPPORTED_APPS if c in available]


# Named generous budgets. Each is far above any non-pathological version of the
# thing it waits for; none is a timing assertion (convention 14).
CONSENT_CARD_BUDGET_S = 90.0
"""For the offer to cross the peer channel and paint as a consent card."""

ADMISSION_BUDGET_S = 120.0
"""For consent → deliver → admit → the machinery write-through to land."""

POLL_S = 0.5


def _seat(nest, app_path, app_name, name, request):
    """One signed-in seat on its own account.

    Each launch keeps the driver's DEFAULT per-launch world (convention 10) —
    no shared xdg base, no shared keyring namespace — so the two seats are two
    installs of one app rather than two instances of one install. That is both
    the honest shape (two people, two machines, collapsed onto one for the
    test) and what keeps the instance lock out of the way.
    """
    from actions import ActionLayer
    from common.auth import create_actor_and_register

    actor = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"]
    )
    driver = create_driver(app_name)
    # ⚠ The R14 (account-data-plane.md § The ratified decisions) trust seed is NOT optional here, and this fixture launches
    # outside the shared `app` fixture that normally applies it. A plaintext
    # e2e nest can never graduate the TLS channel-binding pin
    # `trust::trusted_escrow_holders` reads, so without the seed no generation
    # tip resolves, every fleet-only sealed write refuses — and this ceremony's
    # held-root and reception-key rows are exactly that
    # (`e2e-automation-surface-gating.md` § The e2e trust seed).
    environment = {
        "FAUNA_CONV_POLL_SECS": "2",
        # Scoped to the crates this file's failure modes live in — both the
        # app crate (whichever app this seat is) and the shared ceremony
        # crates. Under the driver, stderr is a plain FILE, so the stderr
        # layer is live and `app_stderr_text()` can quote it into an
        # assertion — which is the only way a two-process ceremony failure
        # names its own side.
        "RUST_LOG": (
            f"info,fauna_{app_name}=debug,fauna_client_capabilities=debug,"
            "fauna_peer_share=debug,fauna_peer_channel=debug,fauna_iroh=debug"
        ),
    }
    _apply_r14_trust_env(environment, nest, request)
    driver.launch({"app_path": app_path, "url": nest["url"], "environment": environment})
    driver.set_state(
        {
            "session": {
                "authenticated": True,
                "node_url": nest["url"],
                "secret_hex": bytes(actor["signing_key"]).hex(),
                "actor_id": actor["actor_id_hex"],
                # A real 32-byte device id (`test_devices_conflicts.py`'s own
                # convention), not a readable label: apple's account-runtime
                # host (`FaunaClient.startAccountRuntime`, landed 2026-08-25)
                # hex-decodes this field and FAILS BY DESIGN on a wrong-length
                # result rather than degrading — the same ruling
                # `index_lease_device` already carries. A label like
                # "e2e-offline-share-initiator" decodes to zero bytes (its
                # non-hex characters abort the parse), which the ceremony's
                # `initiate`/`consent` doors then hard-refuse with "this
                # device has no account runtime yet" — invisible on every
                # OTHER macOS/iOS e2e surface because nothing else requires
                # the runtime to have started.
                "device_id": os.urandom(32).hex(),
            },
            "nav": {"stack": [{"view": "feed"}]},
        }
    )
    return ActionLayer(driver), actor


@pytest.fixture(params=_offline_share_apps())
def offline_share_app(request):
    """The app under test; its id lands in the test name (``[tui]``/
    ``[linux]``), which is what conftest's ``--app`` filter reads. Both seats
    of a run share this one app — the ceremony is symmetric, and a
    tui-initiator/linux-recipient mix is a different (unwritten) journey, not
    this one."""
    return request.param


@pytest.fixture()
def ceremony_seats(nest_instance, offline_share_app, request):
    """Two same-app seats, two accounts, one nest — the co-present pair.

    Two separate launches rather than two accounts in one app: the ceremony
    dials an **actor-keyed** endpoint, so one process holding both identities
    would prove nothing about the dial. Each launch gets the driver's private
    HOME/XDG world (convention 10), so the two seats share no keyring, no
    config and no agent socket.
    """
    app_path = request.getfixturevalue(f"{offline_share_app}_app_path")

    seats = []
    try:
        initiator, initiator_actor = _seat(
            nest_instance, app_path, offline_share_app, "initiator", request
        )
        seats.append(initiator)
        recipient, recipient_actor = _seat(
            nest_instance, app_path, offline_share_app, "recipient", request
        )
        seats.append(recipient)
        yield (initiator, initiator_actor, recipient, recipient_actor)
    finally:
        for seat in seats:
            try:
                seat.driver.teardown()
            except Exception:
                pass


def _await_card_or_dial_failure(initiator, recipient):
    """Race the recipient's consent card against the initiator's own failure,
    and decide which of the two the run is looking at.

    ⚠ This MUST be a race and not a sample. `Op::BeginOfflineShare` is declared
    `outlives_click` (it waits on another human), so the click returns before
    the dial has even been attempted — reading the initiator's status straight
    after it always sees `Idle`, which is exactly how the first version of this
    gate let a known failure fall through into a 90-second card timeout.

    Whichever lands first decides: the card → return, and the rest of the test
    asserts the row's real success; the initiator's terminal `Failed` → red,
    loudly, with both seats quoted.

    🪦 **The no-discovery `skip_unbuilt` branch is GONE (2026-08-19).** It was
    the sanctioned form for real temporary debt while the co-present dial had
    no addressing information, and it self-lifted the moment the compare code
    started carrying LAN endpoints (`p2p.md` § Offline share initiation →
    contract point 1, *The compare code carries the addressing*; journey green
    4/4 the same day). Deleting it rather than leaving it dormant is the
    point: a signature-gated skip that outlives its debt turns the NEXT
    addressing regression into a silent skip instead of a red, which is
    exactly how a covered mechanism quietly stops being covered. If addressing
    ever breaks again this must fail.
    """
    deadline = time.monotonic() + CONSENT_CARD_BUDGET_S
    while True:
        if _consent_card_count(recipient) == 1:
            return
        if (
            initiator.backups.offline_share_status()
            == S.folders.offline_share_status_failed
        ):
            break
        if time.monotonic() >= deadline:
            raise AssertionError(
                "neither the consent card nor a reported failure arrived within "
                f"{CONSENT_CARD_BUDGET_S}s — the ceremony is stuck, which is "
                "neither the known gap nor a clean red:\n"
                + _seat_state("initiator", initiator)
                + "\n"
                + _seat_state("recipient", recipient)
            )
        time.sleep(POLL_S)  # sleep-ok: sampling interval inside a deadline poll (convention 14 mechanism 1) — the assertion is on state and the budget bounds it; this only decides how finely the state is sampled

    error = initiator.error_text() or ""
    log = ""
    try:
        log = initiator.driver.app_stderr_text() or ""
    except Exception:
        pass
    hint = ""
    if "addressing information" in f"{error}\n{log}".lower():
        hint = (
            "\n\n⚠ This is the 2026-08-18 signature: the dial had no path. It is "
            "supposed to be impossible now — the compare code carries the "
            "initiator's LAN endpoints. Check that the seat recorded its bound "
            "addresses (`CeremonyNode::with_bound_addrs`, fed from "
            "`fauna_iroh::peer_leg_transport`'s second return value) and that the "
            "code the recipient typed still has its `-`-separated candidate "
            "groups. p2p.md § Offline share initiation, contract point 1."
        )
    raise AssertionError(
        "the initiator's ceremony failed:\n"
        + _seat_state("initiator", initiator)
        + "\n"
        + _seat_state("recipient", recipient)
        + hint
    )


def _seat_state(label, app):
    """One seat's whole visible story, for a failure message.

    A two-process ceremony fails on ONE side, and the side that shows nothing
    is usually not the side that broke: an initiator whose dial failed leaves
    the recipient with a perfectly clean, perfectly empty page. So every
    failure here quotes both seats — status, error element, and the tail of
    the app's own log (convention 6: the failure diagnoses itself).
    """
    try:
        status = app.backups.offline_share_status()
    except Exception as e:  # the element may not be painted at all
        status = f"<unreadable: {e}>"
    try:
        error = app.error_text()
    except Exception as e:
        error = f"<unreadable: {e}>"
    # The code carries where this seat said it can be reached, so a dial that
    # never lands names the candidates it was aimed at.
    try:
        code = app.backups.offline_share_own_code()
    except Exception as e:  # the panel may be closed by the time we look
        code = f"<unreadable: {e}>"
    try:
        log = app.driver.app_stderr_text() or ""
    except Exception as e:
        log = f"<unreadable: {e}>"
    interesting = [
        plain
        for plain in (strip_ansi(raw) for raw in log.splitlines())
        if _is_ceremony_evidence(plain)
    ]
    tail = "\n      ".join(interesting[-12:]) or "(nothing in the log)"
    return (
        f"  {label}: status={status!r} error={error!r}\n"
        f"      code={code!r}\n      {tail}"
    )


_CEREMONY_KEYS = ("offline-share", "ceremony", "dial", "peer", "group")


def _is_ceremony_evidence(line):
    """Whether one ANSI-stripped log line belongs in a failure's quoted tail.

    A level counts as tracing's level TOKEN, never a bare substring: a routine
    `fauna_devices` refresh line carries `error=false`, and matching "error" in
    it filled tui's whole tail with refreshes, pushing the recipient's own
    "ceremony frame recorded" lines out of the one message that could have
    named the cause (2026-09-15).
    """
    if any(k in line.lower() for k in _CEREMONY_KEYS):
        return True
    return any(f" {level} " in line for level in ("WARN", "ERROR"))


def _await(what, predicate, budget, *, seats=()):
    """Poll `predicate` to a deadline, then fail with both seats' state.

    The reads happen at failure time on purpose — reading them every poll
    would turn a transient into the headline, and quoting a log tail per poll
    would drown the run.
    """
    deadline = time.monotonic() + budget
    while True:
        result = predicate()
        if result:
            return result
        if time.monotonic() >= deadline:
            story = "\n".join(_seat_state(label, app) for label, app in seats)
            raise AssertionError(
                f"{what} did not happen within {budget}s\n{story}"
            )
        time.sleep(POLL_S)  # sleep-ok: sampling interval inside a deadline poll (convention 14 mechanism 1) — the assertion is on state and the budget bounds it; this only decides how finely the state is sampled


def _consent_card_count(app):
    """How many `folder-pending-share` cards the page paints — re-navigating
    first, because both knock lists are fetched on page-visible rather than
    pushed (the shipped posture for the M2 list, which the ceremony's arm
    joins rather than diverging from)."""
    app.backups.navigate_folders()
    return app.driver.count("folder-pending-share")


def _folder_row_texts(app):
    app.backups.navigate_folders()
    return _folder_row_texts_in_place(app)


def _folder_row_texts_in_place(app):
    """The folder rows the page shows RIGHT NOW, never re-navigating.

    Begin and Accept are both pressed on the Folders page, and a landed scope
    must list on that page without the user leaving it — the page re-reads its
    group listing on that edge (`CeremonyStatus::lands_a_scope`). A navigating
    read re-fetches the listing, and would hide exactly that gap.
    """
    return [
        app.driver.get_text("folder-row", index=i)
        for i in range(app.driver.count("folder-row"))
    ]


def _require_affordance(*seats):
    """Skip ONLY for the one legitimate cause, and prove it is the cause.

    ⚠ `skip_environment` on a bare "the button is not there" is the vacuous-pass
    trap: an app that failed to sign in paints no folders page either, and the
    file would then pass forever while testing nothing. So the page itself is
    asserted first — `folder-add-button` is unconditional on a signed-in
    Folders page — and only a page that is demonstrably THERE and still missing
    the affordance counts as the nest declining to advertise `p2p-share`.
    """
    for i, app in enumerate(seats):
        assert app.driver.is_visible("folder-add-button"), (
            f"seat {i} is not on a signed-in Folders page at all, so a "
            f"'capability absent' skip would be a lie; error={app.error_text()!r}"
        )
    if not all(app.backups.offline_share_available() for app in seats):
        skip_environment(
            "this nest does not advertise the p2p-share capability, so no "
            "ceremony listener may bind (p2p.md § Wormability posture, rule 7)"
        )


@pytest.mark.feature("offline-sharing")
def test_a_co_present_ceremony_lists_the_shared_set_on_both_seats(ceremony_seats):
    """The whole ceremony, as two people would do it.

    Every link is asserted separately so a red names itself rather than landing
    as one opaque "the folder never appeared".
    """
    initiator, initiator_actor, recipient, recipient_actor = ceremony_seats

    initiator.backups.navigate_folders()
    recipient.backups.navigate_folders()
    _require_affordance(initiator, recipient)

    # ── The in-person compare. Each device shows its own key; each person
    # types the other's. The codes are read out of the LIVE apps rather than
    # taken from the fixtures, because that equality is the ceremony's only
    # defence against a man-in-the-middle.
    initiator.backups.open_offline_share()
    initiator_code = initiator.backups.offline_share_own_code()
    # The code LEADS with the actor key and then carries this seat's
    # addressing (`p2p.md` § Offline share initiation → contract point 1,
    # *The compare code carries the addressing*). The identity half is what
    # defends against a man-in-the-middle, so that is what is asserted; the
    # addressing half is what makes the dial land at all, asserted below.
    assert initiator_code.startswith(initiator_actor["actor_id_hex"])

    recipient.backups.open_offline_receive()
    recipient_code = recipient.backups.offline_share_own_code()
    assert recipient_code.startswith(recipient_actor["actor_id_hex"])
    assert initiator_code != recipient_code
    # A code with no addressing after the key is the shape that could not
    # dial — the 2026-08-18 RED. Assert the seat actually published somewhere
    # to be reached, or this journey would "pass" against the old failure.
    assert len(initiator_code) > len(initiator_actor["actor_id_hex"]), (
        "the initiator's code carries no addressing, so nothing can reach it: "
        f"{initiator_code!r}"
    )

    # ── The receive act FIRST: first-contact admission is the recipient's own
    # expectation, so an offer arriving before it is refused before any payload
    # is parsed (`p2p.md` § Offline share initiation, rule 1). Doing this in
    # the wrong order is not a race this test papers over — it is the design.
    recipient.backups.type_offline_peer_code(initiator_code)
    # Waits on the gate's STATE, not the instant after the typing returns (see
    # `wait_offline_share_begin_enabled`); a miss reports the field, status and
    # error bar so a refused code and a gate that never repainted read apart.
    recipient.backups.wait_offline_receive_expect_enabled()
    recipient.driver.click("offline-receive-expect-button")

    # ── Begin. The op outlives the click (it is waiting on the other person),
    # so this returns while the ceremony is in flight.
    initiator.backups.type_offline_peer_code(recipient_code)
    initiator.backups.wait_offline_share_begin_enabled()
    initiator.driver.click("offline-share-begin-button")

    # ── Link 1: did the initiator's own side survive the dial? This is the
    # failure mode that leaves the RECIPIENT looking perfectly healthy — clean
    # page, empty error, no card — so asserting the card first would blame the
    # wrong seat. `Failed` here is terminal, so this is a fact, not a race.
    # ── Link 2: the offer crosses and knocks — or the dial dies first.
    _await_card_or_dial_failure(initiator, recipient)
    card = recipient.driver.get_text("folder-pending-share", index=0)
    assert initiator_actor["actor_id_hex"][:6] in card, (
        "the card must name who is handing the set over — the only identity a "
        f"user has to judge the offer by. card={card!r}"
    )

    # ── Consent, from the recipient's own page. Also an outlives-click op: it
    # then waits for the deliver.
    recipient.driver.click("folder-share-accept-button", index=0)

    # ── The success condition: the recipient's folders page LISTS the set.
    # Asserted through the page, not through the config record — a ceremony
    # recorded but never adopted must not list. Read in place: Accept was
    # pressed on this page, so the set must list here without leaving it.
    rows = _await(
        "the shared set to list on the recipient's open folders page",
        lambda: _folder_row_texts_in_place(recipient) or None,
        ADMISSION_BUDGET_S,
        seats=(("initiator", initiator), ("recipient", recipient)),
    )
    assert len(rows) == 1, f"exactly the shared set, nothing invented: {rows}"
    # …and it stays listed when the open page is re-selected, which never
    # re-maps it — the edge linux's listing once missed.
    assert _folder_row_texts(recipient) == rows, (
        "re-selecting the open folders page must keep the shared set listed"
    )

    # The badge says whose it is — the recipient did not mint this scope.
    badge = recipient.driver.get_text("folder-shared-badge", index=0)
    assert badge.strip(), "a listed set owes a badge saying where it came from"
    assert initiator_actor["actor_id_hex"][:6] in badge, (
        f"the recipient's badge must name the sharer: {badge!r}"
    )

    # And the consent card is gone: consent is not a thing you answer twice.
    assert _consent_card_count(recipient) == 0, (
        "an answered invitation must stop knocking"
    )

    # ── The initiator's own side landed too. Its machinery write-through is
    # the half that was silently dropped before the second pass, and it
    # is invisible from the recipient's page — so it gets its own assertion.
    initiator_rows = _await(
        "the shared set to list on the initiator's open folders page",
        lambda: _folder_row_texts_in_place(initiator) or None,
        ADMISSION_BUDGET_S,
        seats=(("initiator", initiator), ("recipient", recipient)),
    )
    assert len(initiator_rows) == 1, (
        f"the initiator holds the set it just shared: {initiator_rows}"
    )
    assert initiator.backups.offline_share_status() == S.folders.offline_share_status_delivered, (
        "the initiator's own panel reports the ceremony finished; the REASON a "
        f"ceremony stopped rides error-message, never this element "
        f"(error={initiator.error_text()!r})"
    )


def _refuses_connections(host, port):
    """Whether a TCP connect to `host:port` is refused RIGHT NOW.

    The teeth of the nest-unreachable journey below, and a FACT rather than a
    wait: `stop_nest` returns only once the process has exited, so nothing is
    listening by the time this is read. Without it, a regression that left the
    nest up (a fixture change, a second nest on the port) would turn that
    journey into an ordinary online ceremony that passes while proving the
    opposite of what it claims.
    """
    try:
        with socket.create_connection((host, port), timeout=2.0):
            return False
    except OSError:
        return True


@pytest.mark.feature("offline-sharing")
def test_a_co_present_ceremony_completes_while_the_nest_is_unreachable(
    ceremony_seats, nest_instance
):
    """The whole ceremony again, with the nest DOWN for every step of it.

    `p2p.md` § Offline share initiation: *"The door that binds reads the brake
    at that moment, so the ceremony itself then runs with no nest involved at
    all, which is the substantive offline claim."* The online twin above
    cannot witness that sentence — it runs against a live nest throughout, so
    every step of it is free to have used one.

    **The bound the goal doc sets, and why the panels open FIRST.** The same
    section: *"(A cold start with no nest ever reachable still cannot bind
    through the panel's door — it has no cached capability read.)"* Two steps
    genuinely read the nest: binding the ceremony seat — the rule-7 brake,
    read live by `offline_share::bind_seat`, which opening each panel spends —
    and each device's first generation tip, which the ceremony's rows seal
    under and which resolves only once its escrow receipt lands at the nest.
    So the witness is an **unreachable** nest, never a never-reached one: both
    seats bind and hold a tip while it is up, and it is gone before the first
    ceremony act.

    Everything after that point is asserted with the port refusing
    connections: the receive act, the dial, the offer, the consent card, the
    accept, the deliver, the admission, and both seats' folders pages listing
    the set.
    """
    initiator, initiator_actor, recipient, recipient_actor = ceremony_seats

    initiator.backups.navigate_folders()
    recipient.backups.navigate_folders()
    _require_affordance(initiator, recipient)

    # ── The two reaches, spent while the nest is still there ────────────────
    initiator.backups.open_offline_share()
    initiator_code = initiator.backups.offline_share_own_code()
    recipient.backups.open_offline_receive()
    recipient_code = recipient.backups.offline_share_own_code()
    assert initiator_code.startswith(initiator_actor["actor_id_hex"])
    assert recipient_code.startswith(recipient_actor["actor_id_hex"])
    # A seat that bound published somewhere to be reached. With no nest to
    # fall back on, a code carrying no addressing cannot dial at all, so this
    # is the precondition of everything below rather than a nicety.
    assert len(initiator_code) > len(initiator_actor["actor_id_hex"]), (
        f"the initiator's code carries no addressing: {initiator_code!r}"
    )
    assert len(recipient_code) > len(recipient_actor["actor_id_hex"]), (
        f"the recipient's code carries no addressing: {recipient_code!r}"
    )
    # The other reach, also spent while the nest is up: each device's first
    # generation tip. The ceremony's own rows — the held root, the reception
    # key, the record — seal under it (`p2p.md` § Implementation status today:
    # "the offline ceremony gains no new precondition; a device with no tip
    # yet refuses the flush at the writer door"), and a tip resolves only once
    # its mint's escrow receipt lands AT the nest. Stopping the nest before
    # that races each seat's first account pass: windows' recipient lost it
    # (2026-09-29) and its Accept refused on "no candidate generation tip".
    # Waited on the nest's own state, never a sleep (convention 14).
    await_a_tip_sealed_row(initiator, nest_instance, initiator_actor, where="the e2e nest")
    await_a_tip_sealed_row(recipient, nest_instance, recipient_actor, where="the e2e nest")

    host = nest_instance.get("bind_host", "127.0.0.1")
    port = nest_instance["port"]

    # Everything past here runs under try/finally: `nest_instance` is
    # session-scoped, so a failure that left it down would red every later test
    # in the run on a connect precondition and bury this one's own cause.
    stop_nest(nest_instance, graceful=True)
    try:
        assert _refuses_connections(host, port), (
            f"the nest still answers on {host}:{port} after stop_nest — this "
            "journey would then prove nothing it claims"
        )

        # ── The receive act, offline. First-contact admission is the
        # recipient's own expectation, minted device-locally on the seat both
        # panels share; no nest is party to it.
        recipient.backups.type_offline_peer_code(initiator_code)
        recipient.backups.wait_offline_receive_expect_enabled()
        recipient.driver.click("offline-receive-expect-button")

        # ── Begin, offline. The dial rides the addressing in the code.
        initiator.backups.type_offline_peer_code(recipient_code)
        initiator.backups.wait_offline_share_begin_enabled()
        initiator.driver.click("offline-share-begin-button")

        _await_card_or_dial_failure(initiator, recipient)
        card = recipient.driver.get_text("folder-pending-share", index=0)
        assert initiator_actor["actor_id_hex"][:6] in card, (
            "the card must name who is handing the set over, with or without a "
            f"nest to ask: card={card!r}"
        )

        # ── Consent, offline: reception key rested, accept recorded, deliver
        # awaited, admission run, machinery written through — all of it into
        # the device's own account store.
        recipient.driver.click("folder-share-accept-button", index=0)

        rows = _await(
            "the shared set to list on the recipient's open folders page with "
            "the nest unreachable",
            lambda: _folder_row_texts_in_place(recipient) or None,
            ADMISSION_BUDGET_S,
            seats=(("initiator", initiator), ("recipient", recipient)),
        )
        assert len(rows) == 1, f"exactly the shared set, nothing invented: {rows}"

        badge = recipient.driver.get_text("folder-shared-badge", index=0)
        assert initiator_actor["actor_id_hex"][:6] in badge, (
            f"the recipient's badge must name the sharer: {badge!r}"
        )

        # The initiator's own write-through landed too — invisible from the
        # recipient's page, and the half a nest-mediated read would hide.
        initiator_rows = _await(
            "the shared set to list on the initiator's open folders page with "
            "the nest unreachable",
            lambda: _folder_row_texts_in_place(initiator) or None,
            ADMISSION_BUDGET_S,
            seats=(("initiator", initiator), ("recipient", recipient)),
        )
        assert len(initiator_rows) == 1, (
            f"the initiator holds the set it just shared: {initiator_rows}"
        )
        assert (
            initiator.backups.offline_share_status()
            == S.folders.offline_share_status_delivered
        ), (
            "the initiator's own panel reports the ceremony finished "
            f"(error={initiator.error_text()!r})"
        )

        # Read again at the END: a nest that came back mid-journey would make
        # every assertion above an online result wearing an offline name.
        assert _refuses_connections(host, port), (
            f"the nest came back on {host}:{port} during the ceremony, so "
            "nothing above is evidence of an offline one"
        )
    finally:
        start_nest_in_place(nest_instance)


@pytest.mark.feature("offline-sharing")
def test_a_declined_invitation_lists_nothing_and_stops_knocking(ceremony_seats):
    """The decline leg is terminal, and terminal means BOTH things: the card
    goes away, and no set is ever listed.

    Worth its own journey because the two halves fail independently — a
    decline that dismissed the card but still adopted the machinery would look
    perfectly correct on the page it was pressed on.
    """
    initiator, initiator_actor, recipient, recipient_actor = ceremony_seats

    initiator.backups.navigate_folders()
    recipient.backups.navigate_folders()
    _require_affordance(initiator, recipient)

    initiator.backups.open_offline_share()
    initiator_code = initiator.backups.offline_share_own_code()
    recipient.backups.open_offline_receive()
    recipient_code = recipient.backups.offline_share_own_code()

    recipient.backups.type_offline_peer_code(initiator_code)
    recipient.driver.click("offline-receive-expect-button")

    initiator.backups.type_offline_peer_code(recipient_code)
    initiator.driver.click("offline-share-begin-button")

    _await_card_or_dial_failure(initiator, recipient)

    recipient.driver.click("folder-share-decline-button", index=0)

    # The card is gone — the decline is monotone and fleet-wide, so it never
    # re-knocks on any of this account's devices.
    _await(
        "the declined invitation to stop knocking",
        lambda: _consent_card_count(recipient) == 0,
        CONSENT_CARD_BUDGET_S,
        seats=(("initiator", initiator), ("recipient", recipient)),
    )
    # …and nothing was adopted. Re-read rather than trusting the poll above:
    # this is the half a card-only decline would pass.
    assert _folder_row_texts(recipient) == [], (
        "a declined share must leave no set behind"
    )


@pytest.mark.feature("offline-sharing")
def test_a_stranger_cannot_start_a_ceremony_nobody_asked_for(ceremony_seats):
    """No receive act, no ceremony. First-contact admission is the recipient's
    own expectation, so an uninvited initiator's frames are refused *before any
    payload is parsed* — the strongest thing this affordance claims, and the
    one a journey can check end to end.
    """
    initiator, _initiator_actor, recipient, recipient_actor = ceremony_seats

    initiator.backups.navigate_folders()
    recipient.backups.navigate_folders()
    _require_affordance(initiator, recipient)

    # The recipient opens its panel — so its listener IS bound and reachable —
    # but never presses the receive act. That distinction is the whole test.
    recipient.backups.open_offline_receive()
    recipient_code = recipient.backups.offline_share_own_code()

    initiator.backups.open_offline_share()
    initiator.backups.type_offline_peer_code(recipient_code)
    initiator.driver.click("offline-share-begin-button")

    # The barrier is CAUSAL, not a settle: the refusal travels back over the
    # same channel the offer went out on, so the initiator's own panel reaching
    # `Failed` proves the recipient's listener has already seen and rejected
    # the frame. Only then is "no card appeared" a fact rather than a race.
    _await(
        "the uninvited offer to be refused, on the initiator's own panel",
        lambda: initiator.backups.offline_share_status()
        == S.folders.offline_share_status_failed,
        CONSENT_CARD_BUDGET_S,
        seats=(("initiator", initiator), ("recipient", recipient)),
    )
    assert _consent_card_count(recipient) == 0, (
        "an uninvited offer must never become a consent card — first-contact "
        "admission is the receive act, not the dial"
    )
    assert _folder_row_texts(recipient) == [], (
        "and nothing may be adopted from a refused ceremony"
    )


#: Comfortably past `GROUP_CEREMONY_EXPECTATION_TTL_SECS` (15 min), and read
#: as "the sitting is over", never as a timing assertion: the window is a Rust
#: constant, so any offset beyond it produces the same state.
LAPSED_BY_S = 20 * 60


@pytest.mark.feature("offline-sharing")
def test_a_receive_window_that_lapsed_refuses_the_initiator_like_a_stranger(
    ceremony_seats,
):
    """Being ready to receive lasts only a short while.

    `p2p.md` § Offline share initiation: the receive act "mints a device-local,
    single-actor, 15-minute-TTL expectation … that admits exactly the scanned
    initiator's ceremony frames **before any payload parsing**". So an
    initiator arriving after the sitting is over must be refused exactly as an
    uninvited one is — the recipient having *once* been ready is not a standing
    invitation.

    **The TTL is a Rust constant and never a knob** (the goal doc is explicit:
    a co-present ceremony either happens within the sitting that minted the
    expectation or is re-initiated by re-scanning), so the only way to reach
    the lapse is to move the recipient's admission clock — convention 14's fake
    clock, never a sleep, and never a configurable window. The seam is
    `fauna_sync_engine::ceremony_clock`, compiled out of release artifacts
    (convention 15) and read per call by the `NowFn` a seat is bound with, so
    it moves a listener that is already up.

    **Why this cannot pass vacuously.** Refused-and-no-card is also what a
    ceremony that never worked at all looks like, so the assertions alone would
    be satisfied by a broken dial. The test therefore carries its own control:
    after the refusal it puts the clock back and dials AGAIN, on the same two
    seats, against the SAME expectation — never re-pressing the receive act —
    and that dial must land a consent card. Only the clock differs between the
    two halves, so the refusal is attributable to the lapse and to nothing
    else.
    """
    initiator, initiator_actor, recipient, recipient_actor = ceremony_seats

    initiator.backups.navigate_folders()
    recipient.backups.navigate_folders()
    _require_affordance(initiator, recipient)

    initiator.backups.open_offline_share()
    initiator_code = initiator.backups.offline_share_own_code()
    recipient.backups.open_offline_receive()
    recipient_code = recipient.backups.offline_share_own_code()

    # ── The receive act: the window opens here, at the recipient's own clock.
    recipient.backups.type_offline_peer_code(initiator_code)
    recipient.backups.wait_offline_receive_expect_enabled()
    recipient.driver.click("offline-receive-expect-button")

    # ── The sitting ends. Only the RECIPIENT's clock moves — admission is its
    # decision, and leaving the initiator on the real clock keeps the offer
    # itself ordinary.
    recipient.backups.advance_offline_share_clock(LAPSED_BY_S)
    try:
        initiator.backups.type_offline_peer_code(recipient_code)
        initiator.backups.wait_offline_share_begin_enabled()
        initiator.driver.click("offline-share-begin-button")

        # The barrier is CAUSAL, exactly as in the uninvited-initiator journey:
        # the refusal travels back over the channel the offer went out on, so
        # the initiator's own panel reaching `Failed` proves the recipient's
        # listener has already seen and rejected the frame. Only then is "no
        # card" a fact rather than a race.
        _await(
            "the late offer to be refused, on the initiator's own panel",
            lambda: initiator.backups.offline_share_status()
            == S.folders.offline_share_status_failed,
            CONSENT_CARD_BUDGET_S,
            seats=(("initiator", initiator), ("recipient", recipient)),
        )
        assert _consent_card_count(recipient) == 0, (
            "a lapsed receive window must refuse before any payload is parsed, "
            "so no consent card may appear"
        )
        assert _folder_row_texts(recipient) == [], (
            "and nothing may be adopted from a refused ceremony"
        )
    finally:
        # Process-wide and nothing auto-resets it: a leftover offset would
        # lapse the next expectation this app mints, in this test or a later
        # one sharing the process.
        recipient.backups.advance_offline_share_clock(0)

    # ── The control. Same seats, same expectation — the receive act is NOT
    # pressed again — and the clock back where it was. This must now land a
    # card, which is what makes the refusal above the lapse's doing.
    initiator.backups.wait_offline_share_begin_enabled()
    initiator.driver.click("offline-share-begin-button")
    # Deliberately NOT `_await_card_or_dial_failure`: that gate treats the
    # initiator's `Failed` as terminal, and this panel is ALREADY `Failed`
    # from the refusal above — it would read the stale status and blame a dial
    # that has not been attempted yet. The card is the only signal here.
    _await(
        "the control dial to land a consent card from the same expectation",
        lambda: _consent_card_count(recipient) == 1,
        CONSENT_CARD_BUDGET_S,
        seats=(("initiator", initiator), ("recipient", recipient)),
    )
    card = recipient.driver.get_text("folder-pending-share", index=0)
    assert initiator_actor["actor_id_hex"][:6] in card, (
        "the control dial must produce a real card from the same expectation "
        f"the lapse refused: card={card!r}"
    )


@pytest.mark.feature("offline-sharing")
def test_a_connection_dropped_part_way_picks_the_same_share_up_again(ceremony_seats):
    """The link between the two devices fails part-way, and the share still
    finishes, with nobody entering a code a second time.

    `p2p.md` § Offline share initiation: "from offer-recorded on, the durable
    ceremony record itself admits, so a dropped co-present connection redials
    without re-scanning." The initiator's walk dials the code it already has
    again (`GroupShareInitiator::with_redial`), and the recipient lets it back
    in because its own record of the offer says who this is.

    **What makes this a witness of the RECORD, not of the receive act.** The
    recipient closes its receive panel once the card is showing. Cancel
    withdraws the receive act's expectation (rule 6), so from that moment only
    the record can admit anyone, and the receive act is never pressed again.
    Only then are the recipient's connections dropped.

    **Why it cannot pass vacuously.** Three facts, each checked: there WAS a
    connection to drop (the agent arm counts it); the initiator really dialed
    again (its own log says so), so the accept and the deliver crossed a
    connection made after the drop; and exactly one set lists on each side, so
    the ceremony that finished is the one that was interrupted, not a second
    one begun afresh.
    """
    initiator, initiator_actor, recipient, recipient_actor = ceremony_seats

    initiator.backups.navigate_folders()
    recipient.backups.navigate_folders()
    _require_affordance(initiator, recipient)

    initiator.backups.open_offline_share()
    initiator_code = initiator.backups.offline_share_own_code()
    recipient.backups.open_offline_receive()
    recipient_code = recipient.backups.offline_share_own_code()

    recipient.backups.type_offline_peer_code(initiator_code)
    recipient.backups.wait_offline_receive_expect_enabled()
    recipient.driver.click("offline-receive-expect-button")

    initiator.backups.type_offline_peer_code(recipient_code)
    initiator.backups.wait_offline_share_begin_enabled()
    initiator.driver.click("offline-share-begin-button")

    # The offer crossed and is on record: the card is the recipient's proof.
    _await_card_or_dial_failure(initiator, recipient)

    # ── The receive act is withdrawn. Cancel closes the panel and takes the
    # expectation with it, so the record is all that admits from here on.
    recipient.driver.click("offline-share-cancel-button")
    assert recipient.driver.is_absent("offline-share-own-code"), (
        "the receive panel must be closed, so no second receive act is possible"
    )
    assert _consent_card_count(recipient) == 1, (
        "closing the receive panel withdraws readiness for NEW offers; the "
        "offer already on record must keep its card"
    )

    # ── The link drops while the initiator is waiting for an answer.
    dropped = recipient.backups.drop_offline_share_connections()
    assert dropped >= 1, (
        "there was no open connection to drop, so nothing below would be a "
        f"redial: dropped={dropped}"
    )

    # ── The recipient answers the card it already has.
    recipient.driver.click("folder-share-accept-button", index=0)

    rows = _await(
        "the shared set to list on the recipient after the dropped connection",
        lambda: _folder_row_texts_in_place(recipient) or None,
        ADMISSION_BUDGET_S,
        seats=(("initiator", initiator), ("recipient", recipient)),
    )
    assert len(rows) == 1, f"exactly the interrupted share, nothing more: {rows}"
    initiator_rows = _await(
        "the shared set to list on the initiator after the dropped connection",
        lambda: _folder_row_texts_in_place(initiator) or None,
        ADMISSION_BUDGET_S,
        seats=(("initiator", initiator), ("recipient", recipient)),
    )
    assert len(initiator_rows) == 1, (
        f"the initiator finished the same share, not a second one: {initiator_rows}"
    )
    assert (
        initiator.backups.offline_share_status()
        == S.folders.offline_share_status_delivered
    ), f"the initiator's panel reports it finished (error={initiator.error_text()!r})"

    # The accept and the deliver crossed a connection made AFTER the drop.
    log = strip_ansi(initiator.driver.app_stderr_text() or "")
    assert "redialed; picking the ceremony up again" in log, (
        "the initiator never dialed again, so this run did not exercise a "
        "dropped connection:\n" + _seat_state("initiator", initiator)
    )
    assert _consent_card_count(recipient) == 0, "an answered invitation stops knocking"


@pytest.mark.feature("offline-sharing")
def test_the_two_seats_never_show_each_other_their_own_code(ceremony_seats):
    """A guard on the compare itself: each app shows ITS OWN key, and refuses
    the other's device to be entered as if it were this one's.

    Cheap, but it is the assertion that would catch a seat rendering a cached
    or shared code — which would make every comparison in this file pass while
    proving nothing.
    """
    initiator, initiator_actor, recipient, recipient_actor = ceremony_seats

    initiator.backups.navigate_folders()
    recipient.backups.navigate_folders()
    _require_affordance(initiator, recipient)

    initiator.backups.open_offline_share()
    recipient.backups.open_offline_receive()

    # Each code LEADS with its own seat's key; the addressing that may follow
    # is this device's, not a shared or cached value.
    assert initiator.backups.offline_share_own_code().startswith(
        initiator_actor["actor_id_hex"]
    )
    assert recipient.backups.offline_share_own_code().startswith(
        recipient_actor["actor_id_hex"]
    )

    # Each refuses ITS OWN code as a counterpart — the self-dial mis-paste.
    initiator.backups.type_offline_peer_code(initiator_actor["actor_id_hex"])
    assert not initiator.backups.offline_share_begin_enabled()
    recipient.backups.type_offline_peer_code(recipient_actor["actor_id_hex"])
    assert not recipient.backups.offline_receive_expect_enabled()
