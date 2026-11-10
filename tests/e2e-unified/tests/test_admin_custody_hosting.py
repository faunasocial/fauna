"""Admin Held-Custody registry (`admin-custody-hosting`) — the page itself,
apart from the ceremony that plants rows for real.

docs/goal/architecture/account-replica-posture.md § The custody grant +
ceremony (the two-sided bounds bullet: the admin list + remove, and the page
telling "not yet answered" from "answered, nothing held").

`test_custody_ceremony_journey.py`'s `test_custody_ceremony_nest_anchored`
lists and removes a hosting row a real two-account ceremony planted — but the
ceremony needs the mint and the accept, which web lacks, so it is marked for
the six apps that run it. The admin page is built on all seven, so this file
drives the page alone against rows planted over the host's own register door
(`helpers.custody_hosting`), and runs on every app.

The module's FIRST test is the held-read one, by design: it needs the app to
open the page with no answer already in hand, and the app is relaunched cold
per module (convention 10), so only the module's first visit to the page is
guaranteed to have nothing to remember.

tier_3 (full stack): a real `fauna-nest` binary + a real admin client over
WS-RPC.
"""

from __future__ import annotations

import re

import pytest
from nacl.signing import SigningKey

from common.auth import create_actor_and_register
from helpers.custody_hosting import UNANSWERED_OWNER_NEST_URL, Host, custody_hosting_hook, pump
from helpers.rpc_hold import arm_rpc_hold, release_rpc_hold, wait_for_held_rpc
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

#: The page's registry read (`fauna-client-capabilities`
#: `custody_hosting.rs`, the admin client's list).
_LIST_KIND = "fauna.admin.custody_hosting.list"

#: A registry read and its re-render — far above any non-pathological delay
#: (convention 14: a named budget a green run never spends).
_ANSWER_S = 30.0


# Marked for the six apps that paint the page while its read is in flight; tui
# is not among them. tui's automation AWAITS an admin sub-page's read before it
# acks the nav or the click that opened it, and its element reads run on the
# same loop, so with the read held there is no tui page to read — a mark, never
# a declared absence (feature-catalog.md § Cell semantics, the marked-witness
# rule). tui satisfies the rule structurally (nothing paints before the answer);
# its own witness is the next test, which reads the page's answered state off
# `/app/state`, the one channel that answers off the loop.
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("admin-held-custody")
def test_the_page_says_nothing_is_held_only_once_the_nest_has_answered(admin_app, nest_instance):
    """While the registry read is parked at the nest the page shows neither a
    count, nor the "nothing held" line, nor a row; once the nest answers, the
    answer is what it shows. An admin who reads "nothing held" is reading the
    nest's answer, never a page that has not loaded."""
    app = admin_app
    port = nest_instance["port"]

    arm_rpc_hold(port, _LIST_KIND)
    try:
        # By its rail row, not the nav patch — the patch waits for the read it
        # triggers on some apps (`AdminActions.open_custody_hosting`).
        app.admin.open_custody_hosting()
        # The read reached the nest and is parked there: whatever the page
        # shows now is its not-yet-answered paint (arrival, not a sleep).
        wait_for_held_rpc(port, _LIST_KIND, diagnose=lambda: f"error: {app.error_text()!r}")
        assert not app.admin.hosting_registry_answered(), (
            "the page must not say how much is held — nor that nothing is — "
            "before the nest has answered: "
            f"{app.driver.diagnose('admin-custody-hosting-empty')} "
            f"{app.driver.diagnose('admin-custody-hosting-count')}"
        )
        assert app.admin.hosting_row_count() == 0, "no row can render before the answer"
    finally:
        release_rpc_hold(port, _LIST_KIND)

    wait_until(
        app.admin.hosting_registry_answered,
        _ANSWER_S,
        diagnose=lambda: f"the released read never rendered; error: {app.error_text()!r}",
    )
    assert not app.error_text(), f"unexpected error: {app.error_text()!r}"


# Second in the module on purpose: tui does not run the test above, so this is
# tui's FIRST visit to the page after the per-module cold relaunch — the only
# visit guaranteed to have no answer in hand (the module docstring). The
# precondition below makes a reorder fail by name rather than pass vacuously.
@pytest.mark.tui
@pytest.mark.feature("admin-held-custody")
def test_tui_names_no_count_while_the_registry_read_is_held(admin_app, nest_instance):
    """tui's witness of the same rule: while the registry read is held at the
    nest the page reports no answer — and a read the client gives up on (its
    own deadline) is not an answer either; once released and re-read, it is.

    tui awaits the sub-page's read before it acks the click that opened it, and
    its element reads run on that same loop, so the gesture is driven from a
    thread and the main thread observes `/app/state` only — the one channel
    that answers off the loop (`data.admin_custody_hosting`, published from
    the same rows the page paints its count from). The privacy twin's shape
    (`test_settings.py::test_privacy_nav_names_no_mode_while_the_mode_read_is_held`).
    """
    import threading

    app = admin_app
    driver = app.driver
    port = nest_instance["port"]

    def _answer() -> dict:
        seen = driver.get_state("data.admin_custody_hosting")
        assert isinstance(seen, dict), (
            f"tui's /app/state must publish the page's answered state; got {seen!r}"
        )
        return seen

    assert _answer() == {"answered": False, "row_count": None}, (
        "precondition: tui has already been answered by this nest, so this run "
        "cannot observe the pre-answer window — the test must stay tui's first "
        f"visit to the page in this module; state={_answer()!r}"
    )

    arm_rpc_hold(port, _LIST_KIND)
    open_error: list[BaseException] = []
    try:
        def _open():
            try:
                app.admin.open_custody_hosting()
            except BaseException as e:  # surfaced after the join below
                open_error.append(e)

        opener = threading.Thread(target=_open, name="custody-open", daemon=True)
        opener.start()

        wait_for_held_rpc(
            port,
            _LIST_KIND,
            diagnose=lambda: f"app-side: nav={driver.get_state('nav')!r} page={_answer()!r}",
        )
        seen = _answer()
        assert seen == {"answered": False, "row_count": None}, (
            "while the registry read is held at the nest, tui must not say how "
            f"much is held — nor that nothing is — but it reports {seen!r}"
        )

        # Still held: the gesture acks only on the client's own deadline, and
        # a read that gave up has not been answered by the nest.
        opener.join(_ANSWER_S)
        assert not opener.is_alive(), (
            f"the opening gesture never acked within {_ANSWER_S}s while held"
        )
        assert not open_error, f"the opening gesture failed to ack: {open_error[0]!r}"
        # Read the page's state from the snapshot that carries the deadline's
        # error, so it is the post-read paint and never a stale pre-read one.
        state = driver.get_state(
            wait_for=lambda s: bool((s.get("messages") or {}).get("error")),
            timeout=_ANSWER_S,
        ) or {}
        assert (state.get("messages") or {}).get("error"), (
            f"the given-up read surfaced no error; state messages={state.get('messages')!r}"
        )
        seen = (state.get("data") or {}).get("admin_custody_hosting")
        assert seen == {"answered": False, "row_count": None}, (
            "a registry read the client gave up on is not the nest's answer, so "
            f"the page must still name no count, but tui reports {seen!r}; "
            f"error: {state['messages']['error']!r}"
        )
    finally:
        release_rpc_hold(port, _LIST_KIND)

    # The abandoned read's late reply has nowhere to go, so the page is
    # re-entered to converge on the nest's real (empty) answer.
    wait_until(
        lambda: (app.admin.reopen_custody_hosting(), _answer())[1]
        == {"answered": True, "row_count": 0},
        _ANSWER_S,
        diagnose=lambda: f"released, the page never answered: {_answer()!r}; error: {app.error_text()!r}",
    )
    assert app.admin.hosting_registry_answered(), "the answered state and the paint must agree"


@pytest.mark.feature("admin-held-custody")
def test_admin_custody_hosting_page_reachable_and_honestly_empty(admin_app):
    """A fresh nest's registry is empty, and the page says so ONLY once the
    read has answered — never before. Pre-hydrate must show neither a count
    nor the empty state; only an ANSWERED empty list says "nobody asked".

    All 7 apps now own this leg (windows landed last) — the per-app skip_unbuilt gate this test carried is retired."""
    app = admin_app

    app.admin.navigate_custody_hosting()
    wait_until(
        lambda: app.admin.hosting_registry_answered(),
        15.0,
        diagnose=lambda: f"error: {app.error_text()!r}",
    )

    assert app.admin.hosting_row_count() == 0, (
        "a fresh nest has no hosting rows — genuine planted-row coverage "
        "lives in test_an_admin_lists_and_removes_a_copy_another_member_holds"
    )
    assert not app.error_text(), f"unexpected error: {app.error_text()!r}"


def _nest_side(nest_instance, app) -> str:
    """What the nest itself lists to its admin, and where the app is — the
    two facts that split "the nest never held the row" from "the page never
    re-read it" (convention 6)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    try:
        with WsRpcAdminClient(nest_instance["url"], actor_id=bytes(admin["signing_key"].verify_key),
                              signing_key=bytes(admin["signing_key"])) as ws:
            rows = ws.call("fauna.admin.custody_hosting.list", {}).get("rows", [])
        listed = [r.get("host_actor_id", "")[:8] for r in rows]
    except Exception as e:  # noqa: BLE001 — diagnostics must not mask the assert
        listed = f"(admin list failed: {e!r})"
    return f"the nest lists hosts {listed}; app nav {app.driver.get_state('nav')!r}"


@pytest.fixture
def planted_hosting_row(nest_instance):
    """A member of this nest (the HOST) has asked it to hold a copy for
    someone elsewhere: one row deposited over the host's own register door.
    The owner is a bare key — the row names an owner nest nothing answers at,
    so it is listed and never pulled. Whatever the test leaves is removed at
    teardown, so the shared nest is left empty for the next test."""
    host_actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"],
        base_url=nest_instance["url"])
    owner_key = SigningKey.generate()
    owner = {"signing_key": bytes(owner_key), "actor_id_hex": bytes(owner_key.verify_key).hex()}
    with Host(nest_instance, host_actor, owner_nest_url=UNANSWERED_OWNER_NEST_URL) as host:
        grant = host.plant(owner)
        yield {"host": host, "grant": grant, "owner": owner}


@pytest.mark.feature("admin-held-custody")
def test_an_admin_lists_and_removes_a_copy_another_member_holds(
    admin_app, planted_hosting_row, nest_instance
):
    """A copy another member asked the nest to hold is listed for the admin —
    naming that member and the address the nest fetches from — and the admin
    removes it from the page: cancel touches nothing, confirm drops it from
    the list and from the member's own read-back."""
    app = admin_app
    host = planted_hosting_row["host"]
    grant = planted_hosting_row["grant"]
    host_hex = host.actor["actor_id_hex"]

    # What the page last showed — an unanswered read, an answered list without
    # this row, and an error are three different failures (convention 6).
    seen: dict = {}

    def _listed() -> int | None:
        app.admin.reopen_custody_hosting()
        seen.update(
            answered=app.admin.hosting_registry_answered(),
            hosts=[app.admin.hosting_row_host(i) for i in range(app.admin.hosting_row_count())],
            error=app.error_text(),
        )
        return app.admin.hosting_row_index(host_hex) if seen["answered"] else None

    wait_until(
        lambda: _listed() is not None,
        _ANSWER_S,
        diagnose=lambda: f"the registry never listed the planted row (host {host_hex[:8]}); "
        f"the page last showed {seen}; {_nest_side(nest_instance, app)}",
    )
    row = app.admin.hosting_row_index(host_hex)
    assert app.admin.hosting_row_url(row) == UNANSWERED_OWNER_NEST_URL, (
        "the row must show the address the nest fetches from, verbatim: "
        f"{app.admin.hosting_row_url(row)!r}"
    )

    # Cancel touches nothing — the confirm is a real gate.
    app.driver.click("admin-custody-hosting-remove-button", index=row)
    app.driver.wait_for("admin-custody-hosting-remove-confirm-button", timeout=10.0)
    app.admin.hosting_cancel_remove()
    assert not app.admin.hosting_remove_armed(), "cancel must disarm the confirm"
    assert app.admin.hosting_row_index(host_hex) is not None, "cancel must remove nothing"

    app.admin.hosting_remove(app.admin.hosting_row_index(host_hex))
    wait_until(
        lambda: _listed() is None and seen["answered"],
        _ANSWER_S,
        diagnose=lambda: f"the admin remove never dropped the row; the page last showed {seen}",
    )
    assert not seen["error"], f"the remove must not surface an error: {seen['error']!r}"
    # The nest dropped it, not just the page (convention 5): the member's own
    # read-back no longer holds it.
    assert grant not in host.rows(), "the admin remove must drop the member's row on the nest"
    host.forget(grant)


# ── What each held copy shows, on copies the pump really filled ─────────────
#
# Last in the module on purpose: its admin session is on `domainless_nest`, not
# the shared nest the tests above read, so it must come after
# `test_tui_names_no_count_while_the_registry_read_is_held`'s first visit.


@pytest.fixture
def custody_spa_url(spa_proxy_for, domainless_nest):
    """The SPA proxy → ``domainless_nest``, for a web leg (resolved lazily by
    `_login_admin_as`, only when the app is web)."""
    return spa_proxy_for(domainless_nest["url"])


@pytest.fixture
def domainless_admin_app(request, app, domainless_nest):
    """``admin_app`` pointed at ``domainless_nest`` — only the nest differs
    (`_login_admin_as`). The pump must DIAL the owner's nest, and the shared
    nest is public by identity, so it refuses the plaintext loopback URL this
    rig hands it (``domainless_nest``'s docstring); copies with real held
    bytes can only be filled here."""
    from conftest import _login_admin_as

    _login_admin_as(
        app, request, domainless_nest,
        spa_url_fixture="custody_spa_url", fixture_name="domainless_admin_app",
    )
    yield app


@pytest.fixture
def held_custody_pump(domainless_nest):
    """``domainless_nest``'s periodic hosting pump held for one test, so the
    only passes are the test's own ``run-now`` pokes (convention 14: state,
    never a race with a scheduled pass)."""
    url = domainless_nest["url"]
    custody_hosting_hook(url, "hold", {"held": True})
    yield domainless_nest
    custody_hosting_hook(url, "hold", {"held": False})


#: `fauna_core::format::byte_size`'s units (1024 steps, one decimal).
_UNITS = {"B": 1, "KB": 1 << 10, "MB": 1 << 20, "GB": 1 << 30, "TB": 1 << 40}


def _shows_bytes(text: str, expected: int) -> bool:
    """Whether ``text`` — the shared byte-size rendering — shows ``expected``
    bytes to its one decimal. Read back rather than re-derived, so the test
    never carries a second copy of the format a client could share a mistake
    with."""
    m = re.fullmatch(r"([0-9]+(?:\.[0-9])?) (B|KB|MB|GB|TB)", text)
    assert m, f"not a shared-format byte size: {text!r}"
    unit = _UNITS[m.group(2)]
    return abs(float(m.group(1)) * unit - expected) <= unit * 0.05 + 1e-9


def _bare_owner() -> dict:
    """An owner who is only a key — for a copy that is listed, never pulled."""
    key = SigningKey.generate()
    return {"signing_key": bytes(key), "actor_id_hex": bytes(key.verify_key).hex()}


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("admin-held-custody")
def test_each_held_copy_shows_its_budget_hold_stop_mark_and_freshness_heaviest_first(
    domainless_admin_app, held_custody_pump
):
    """Each copy on the admin's held-custody page shows its budget, how much
    it holds, whether it is paused and how fresh its last confirmation is, and
    the heaviest hold is listed first.

    docs/goal/architecture/account-data-plane.md § Implementation status today
    (:2066–2078): the tui page renders host / owner / dialled URL / budget /
    metered hold / stop mark / three-state receipt freshness, ordered by
    `view_model::admin_hosting_rows` — heaviest hold first.

    Three copies, each deposited by its own member, so no two share a
    `(host, owner)` store: one the pump fills with no allowance of its own; one
    the pump fills, then squeezes under the allowance set for it, and which its
    member then pauses; one naming an owner nest nothing answers at, so it
    holds nothing and was never confirmed. Every expected cell is read off the
    nest's own row (convention 5), never assumed.

    Marked tui, the lead app the goal doc's claim names, and web, linux,
    android, macos and ios, whose pages read the same shared rows (macos and
    ios through the one FaunaKit `AdminCustodyHostingView`); windows follows
    in its trickle-down leg.
    """
    app = domainless_admin_app
    nest = held_custody_pump
    words = S.admin.custody_hosting

    def _account() -> dict:
        return create_actor_and_register(
            nest["port"], admin_signing_key=nest["admin"]["signing_key"], base_url=nest["url"])

    heavy_owner, light_owner = _account(), _account()
    with (
        Host(nest, _account()) as heavy,
        Host(nest, _account()) as light,
        Host(nest, _account(), owner_nest_url=UNANSWERED_OWNER_NEST_URL) as never,
    ):
        heavy_grant = heavy.custody(heavy_owner)
        light_grant = light.custody(light_owner)
        report = pump(nest)
        assert report["pulled"] == 2 and report["failed"] == 0, (
            f"both filled copies must pull: {report!r}")

        # Squeeze the light copy under its own allowance (outcome 12's
        # mechanism), so the two holds really differ, then pause it.
        allowance = light.rows()[light_grant]["held_bytes"] // 2
        assert allowance > 0, f"the light copy pulled nothing: {light.rows()!r}"
        assert light.register(light_owner, light_grant, cap=allowance).get("ok") is True
        report = pump(nest)
        assert report["pulled"] == 2 and report["failed"] == 0, report
        assert light.register(
            light_owner, light_grant, cap=allowance, stopped=True).get("ok") is True

        # Planted only after the passes, so no pass ever dials it.
        never_grant = never.plant(_bare_owner())

        # The nest's own rows — what the page must show.
        truth = {
            "heavy": heavy.rows()[heavy_grant],
            "light": light.rows()[light_grant],
            "never": never.rows()[never_grant],
        }
        holds = {k: r["held_bytes"] for k, r in truth.items()}
        assert holds["heavy"] > holds["light"] > holds["never"] == 0, (
            f"precondition: three distinct holds, the squeezed copy under the full one: {truth!r}")
        assert truth["light"]["retained_bytes_cap"] == allowance, truth
        assert truth["light"]["stopped"] is True and truth["heavy"]["stopped"] is False, truth
        assert truth["heavy"]["last_receipt_at"] and not truth["never"]["last_receipt_at"], (
            f"precondition: a pulled copy is confirmed, the never-dialled one is not: {truth!r}")
        hosts = {"heavy": heavy, "light": light, "never": never}

        # What the page last showed — an unanswered read, a list missing a
        # copy, and an error are three different failures (convention 6).
        seen: dict = {}

        def _listed() -> dict[str, int | None] | None:
            app.admin.reopen_custody_hosting()
            answered = app.admin.hosting_registry_answered()
            index = {k: app.admin.hosting_row_index(h.actor["actor_id_hex"])
                     for k, h in hosts.items()} if answered else {}
            seen.update(answered=answered, index=index, error=app.error_text())
            return index if index and None not in index.values() else None

        index = wait_until(
            _listed,
            _ANSWER_S,
            diagnose=lambda: f"the page never listed all three copies; it last showed {seen}",
        )
        assert not seen["error"], f"unexpected error: {seen['error']!r}"

        assert index["heavy"] < index["light"] < index["never"], (
            f"the heaviest hold must be listed first: rows {index!r} for holds {holds!r}")

        for k, row in truth.items():
            i = index[k]
            cells = {
                "budget": app.admin.hosting_row_budget(i),
                "held": app.admin.hosting_row_held(i),
                "stopped": app.admin.hosting_row_stopped(i),
                "receipt": app.admin.hosting_row_receipt(i),
            }
            what = f"the {k} copy (nest row {row!r}) shows {cells!r}"

            cap = row["retained_bytes_cap"]
            if cap == 0:
                assert cells["budget"] == words.budget_default, (
                    f"a copy with no allowance of its own reads as the default: {what}")
            else:
                assert _shows_bytes(cells["budget"], cap), f"the budget must read {cap} bytes: {what}"
            assert _shows_bytes(cells["held"], row["held_bytes"]), (
                f"the hold must read the {row['held_bytes']} bytes the pump metered: {what}")
            assert cells["stopped"] == (words.stopped if row["stopped"] else words.active), (
                f"the stop mark must follow the row: {what}")
            assert cells["receipt"] == (
                words.receipt_fresh if row["last_receipt_at"] else words.receipt_none), (
                f"the confirmation's freshness must follow the row: {what}")
