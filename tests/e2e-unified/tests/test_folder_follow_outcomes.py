"""tier_3 — the followed row's own promises, each through the app UI: removing a
follow, a follow whose owner stops publishing, the one plain failure message
(and a network fault that never reads as a revoke), and whose folder a
followed row is.

Owner docs: ``docs/goal/ui/folders.md`` § Following a public folder (the
gesture, the followed row, the *no longer available* state, unfollow),
``docs/goal/behavior/folders.md`` § Publicly-synced follow (the folded
not-found, the flip-back, zero nest-side follower state).

What this file adds over ``test_folder_follow_media_browse.py``: that journey
follows once and asserts the happy ``Following`` status; nothing pressed
``folder-unfollow-button``, watched a follow outlive a revoke, typed a bad
address, cut the network under a follow, or read who a followed row belongs to.

Shape shared by every test here:

* **A dedicated follower** (``follower_app``), never the shared session user —
  a follow lives in the follower's own ``fauna.state.follows`` plane and outlives the test, so on
  the shared user it would leak rows into every later follow journey.
* **The owner is fixture setup** — another person, so their folder, their
  audience flips and their file rows go over the API (the carve-out
  ``test_folder_follow_media_browse.py`` documents); every mutation the
  *follower* makes goes through the UI (convention 8).
* **Rows are addressed by name**, never by index alone: the row list is sorted
  by the follow's pinned identity, not by when it was made.
* **Convention 14** — every wait is a deadline poll on state. The availability
  verdict rides a one-minute staleness budget
  (``public_follow::AVAILABILITY_TTL_MS``), so the revoke/resume waits are
  generous ceilings over a revisit loop, never a sleep sized to the budget.
"""

import secrets

import pytest

import fauna_ffi

from common.auth import create_actor_and_register
from common.nest import start_nest_in_place, stop_nest
from conftest import _login_app_as, _make_user
from helpers.waiting import (
    await_devices_refresh_after,
    devices_refresh_baseline,
    wait_until,
)
from i18n.strings import S

from tests.api.test_public_folder_fetch import DEVICE_ID, _record, _set_audience
from tests.api.test_web_paywall_folder import _actor_client
from helpers.set_names import find_set

pytestmark = [
    pytest.mark.tier_3,
    # tui leads (the lead app); the other apps join by marker as each leg is run
    # against this file — the parity ledger, exactly as the audience test keeps
    # its own.
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
]

# Generous ceilings, not expectations (convention 14).
_UI_WINDOW_S = 20.0
# One staleness budget (60 s) plus a loaded box's headroom: the verdict can only
# move once the cached one expires and a refresh re-probes.
_VERDICT_WINDOW_S = 180.0
# A refresh while the nest is down waits out each read's in-gap deadline in turn
# before it gives up (`transport.md` § Request lifecycle step 3).
_GAP_WINDOW_S = 240.0


@pytest.fixture
def follower_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED fresh actor — the follower."""
    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return app


def _owner(nest_instance) -> dict:
    return create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )


def _folder_for(nest_instance, owner: dict, *, public: bool = True, prefix: str = "pub") -> str:
    """A folder of ``owner``'s, public (with one real file) unless told not."""
    url, port = nest_instance["url"], nest_instance["port"]
    folder = f"{prefix}-{secrets.token_hex(3)}"
    with _actor_client(url, owner) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(owner["signing_key"]),
            {"name": folder},
        )
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    if public:
        _set_audience(url, owner, folder, "public")
        _record(url, port, owner, folder, "notes.txt", b"published on purpose")
    return folder


def _open_follow_form(app) -> None:
    app.backups.navigate_folders()
    app.driver.click("folder-follow-button")
    app.driver.wait_for("folder-follow-name-input", timeout=_UI_WINDOW_S)


def _submit_follow(app, owner_address: str, folder: str) -> None:
    """Fill and press the follow flow — the owner half on the reused
    ``recipient-picker-input``, which takes a handle or a bare actor id."""
    _open_follow_form(app)
    app.driver.clear_and_type("recipient-picker-input", owner_address)
    app.driver.clear_and_type("folder-follow-name-input", folder)
    app.driver.click("folder-follow-confirm")


def _row_texts(app) -> list[str]:
    return [
        app.driver.get_text("folder-followed-item", i)
        for i in range(app.driver.count("folder-followed-item"))
    ]


def _row_of(app, folder: str) -> int | None:
    return next((i for i, text in enumerate(_row_texts(app)) if folder in text), None)


def _status_of(app, folder: str) -> str | None:
    i = _row_of(app, folder)
    if i is None:
        return None
    return app.driver.get_text(
        "folder-followed-status", scope=f"folder-followed-item[{i}]"
    )


def _followed_rows_diagnosis(app) -> str:
    return f"rows={_row_texts(app)!r} error={app.error_text()!r}"


def _follow(app, owner_address: str, folder: str) -> int:
    """Follow through the UI and wait for the row; returns its index."""
    _submit_follow(app, owner_address, folder)
    # Wait on presence, then read the index: the first row is index 0, which
    # `wait_until` would read as not-yet.
    wait_until(
        lambda: _row_of(app, folder) is not None,
        _UI_WINDOW_S,
        diagnose=lambda: _followed_rows_diagnosis(app),
    )
    return _row_of(app, folder)


_FEED_NAV = {"nav": {"stack": [{"view": "feed"}]}}
_FOLDERS_NAV = {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "folders"}]}}


def _revisit_folders(app, *, timeout: float | None = None) -> None:
    """Leave the Folders page and come back — the nav edge is what refreshes
    the page (and, past the staleness budget, re-probes each follow).

    When this returns, a Folders refresh that BEGAN after the revisit has
    COMMITTED — the causal anchor every read below it needs (convention 14),
    observed on the refresh barrier (``fauna_e2e_agent::DEVICES_REFRESHES_KEY``)
    rather than inferred from the nav ack. Only tui's nav ack waits for the
    page's load (``automation.rs::apply_nav``); linux spawns the refresh off the
    page's map hook and web off the section's mount, so on those a read right
    after the ack sees the PRE-visit snapshot, and the network-fault witness
    below would pass on it vacuously. ``timeout`` widens the acks and the
    barrier for a visit whose loads have to wait out a connection gap."""
    baseline = devices_refresh_baseline(app.driver)
    app.driver.set_state(_FEED_NAV, timeout=timeout)
    app.driver.set_state(_FOLDERS_NAV, timeout=timeout)
    await_devices_refresh_after(
        app.driver,
        baseline,
        budget_s=timeout or _UI_WINDOW_S,
        what="the Folders revisit",
    )


def _await_status(app, folder: str, want: str) -> None:
    def _settled():
        _revisit_folders(app)
        return _status_of(app, folder) == want

    wait_until(
        _settled,
        _VERDICT_WINDOW_S,
        interval=2.0,
        diagnose=lambda: (
            f"wanted status {want!r} for {folder!r}; " + _followed_rows_diagnosis(app)
        ),
    )


def _publicly_served(nest_instance, owner: dict, folder: str) -> bool:
    """Whether the owner's nest still serves ``folder`` on the public plane to a
    reader who is neither owner nor follower."""
    stranger = _owner(nest_instance)
    try:
        with _actor_client(nest_instance["url"], stranger) as ws:
            ws.call(
                "fauna.folders.public.fetch",
                {
                    "owner_actor_id": owner["actor_id_hex"],
                    "folder_name": folder,
                    "since": 0,
                },
            )
        return True
    except Exception:  # noqa: BLE001 — the plane's one refusal is the answer
        return False


def _owner_row(nest_instance, owner: dict, folder: str) -> dict | None:
    with _actor_client(nest_instance["url"], owner) as ws:
        reply = ws.call("fauna.folders.list", {})
    return find_set(reply.get("folders", []), folder)


# ── follow-a-public-folder outcome 3 ────────────────────────────────────────


@pytest.mark.feature("follow-a-public-folder")
def test_unfollowing_removes_the_row_with_nothing_to_undo_anywhere(
    follower_app, nest_instance
):
    """Unfollow removes the record locally — nothing to revoke anywhere, because
    the home nest never knew (``ui/folders.md`` § Following a public folder)."""
    app = follower_app
    owner = _owner(nest_instance)
    folder = _folder_for(nest_instance, owner)
    owner_before = _owner_row(nest_instance, owner, folder)

    i = _follow(app, owner["actor_id_hex"], folder)
    app.driver.click("folder-unfollow-button", scope=f"folder-followed-item[{i}]")

    wait_until(
        lambda: _row_of(app, folder) is None,
        _UI_WINDOW_S,
        diagnose=lambda: _followed_rows_diagnosis(app),
    )
    assert not app.error_text(), f"unfollow reported an error: {app.error_text()!r}"

    # Removed from the follower's own record, not merely hidden: a fresh visit
    # re-reads the follows and the row stays gone.
    _revisit_folders(app)
    app.driver.wait_for("folder-follow-button", timeout=_UI_WINDOW_S)
    assert _row_of(app, folder) is None, (
        "the unfollowed folder came back on the next visit — the follow was "
        f"hidden, not removed: {_followed_rows_diagnosis(app)}"
    )

    # …and nothing anywhere else moved: the owner's folder is exactly as it was
    # and still serves the public, so there is nothing for anyone to undo.
    assert _owner_row(nest_instance, owner, folder) == owner_before, (
        "unfollowing must not touch the owner's folder"
    )
    assert _publicly_served(nest_instance, owner, folder), (
        "the folder must still be served publicly after a follower leaves"
    )


# ── follow-a-public-folder outcome 4 ────────────────────────────────────────


@pytest.mark.feature("follow-a-public-folder")
def test_a_follow_says_when_its_owner_stops_publishing_and_resumes_by_itself(
    follower_app, nest_instance
):
    """The flip-back makes the row loud — *no longer available* — and it stays
    until the user removes it; a re-publish resumes it with nothing done by the
    follower (``ui/folders.md`` § Following a public folder; the verdict rule:
    ``behavior/folders.md`` § Publicly-synced follow, *Flip-back closes the plane
    immediately and loudly*)."""
    app = follower_app
    owner = _owner(nest_instance)
    folder = _folder_for(nest_instance, owner)
    _follow(app, owner["actor_id_hex"], folder)
    assert _status_of(app, folder) == S.devices.followed_status_following

    # ── The owner stops publishing. ──
    _set_audience(nest_instance["url"], owner, folder, "private")
    _await_status(app, folder, S.devices.followed_status_unavailable)

    # It says so plainly, and it STAYS — the revoke is not a removal; the user
    # still holds the one affordance that is theirs.
    rows = [text for text in _row_texts(app) if folder in text]
    assert len(rows) == 1, f"the revoked follow must stay listed: {_row_texts(app)!r}"
    i = _row_of(app, folder)
    assert app.driver.count(
        "folder-unfollow-button", scope=f"folder-followed-item[{i}]"
    ) == 1, "a revoked row keeps its remove affordance"

    # ── The owner publishes again: the follow resumes by itself. ──
    _set_audience(nest_instance["url"], owner, folder, "public")
    _await_status(app, folder, S.devices.followed_status_following)


# ── follow-a-public-folder outcome 8 ────────────────────────────────────────


@pytest.mark.feature("follow-a-public-folder")
def test_a_wrong_private_or_gone_address_fails_with_one_plain_message(
    follower_app, nest_instance
):
    """Absent, private and misspelled are folded by the home nest so nothing can
    probe for a sealed folder, and the app says the one ratified sentence for
    all of them (``ui/folders.md`` § Following a public folder; the wording is
    ``devices.error_follow_not_found``)."""
    app = follower_app
    owner = _owner(nest_instance)
    public = _folder_for(nest_instance, owner)
    private = _folder_for(nest_instance, owner, public=False, prefix="priv")
    gone = _folder_for(nest_instance, owner, prefix="gone")
    with _actor_client(nest_instance["url"], owner) as ws:
        ws.call("fauna.folders.delete", {"name": gone})

    attempts = {
        "a misspelled folder name": (owner["actor_id_hex"], public + "x"),
        "a private folder": (owner["actor_id_hex"], private),
        "a deleted folder": (owner["actor_id_hex"], gone),
        "a handle nobody has": (f"nobody-{secrets.token_hex(4)}", public),
    }
    for what, (address, name) in attempts.items():
        # A fresh visit clears the last attempt's message, so each reading below
        # is this attempt's own answer and never a leftover.
        _revisit_folders(app)
        wait_until(
            lambda: not app.error_text(),
            _UI_WINDOW_S,
            diagnose=lambda: f"a stale message stayed up: {app.error_text()!r}",
        )
        _submit_follow(app, address, name)
        message = wait_until(
            app.error_text,
            _UI_WINDOW_S,
            diagnose=lambda what=what: f"{what}: no message; " + _followed_rows_diagnosis(app),
        )
        assert message == S.devices.error_follow_not_found, (
            f"{what} must fail with the one plain message, verbatim — got {message!r}"
        )
        assert _row_of(app, name) is None, f"{what} must not add a row"


@pytest.mark.feature("follow-a-public-folder")
def test_a_network_problem_never_reads_as_the_folder_having_been_taken_away(
    follower_app, nest_instance
):
    """A dropped connection is not an answer about a folder: a follow attempted
    through it says it could not reach the folder, never that there is no such
    folder, and a folder already followed stays listed and ``Following`` —
    never *no longer available*, and never gone from the list (the rule
    ``public_follow::availability_from_probe`` keeps for one verdict, held for
    the whole list by ``rows_when_unreadable``)."""
    app = follower_app
    owner = _owner(nest_instance)
    followed = _folder_for(nest_instance, owner)
    other = _folder_for(nest_instance, owner)
    _follow(app, owner["actor_id_hex"], followed)
    assert _status_of(app, followed) == S.devices.followed_status_following

    nest_down = False
    try:
        stop_nest(nest_instance, graceful=True)
        nest_down = True

        # ── 1. A follow through the gap: an address that WOULD resolve. ──
        # Everything up to the press is local (the form, the two boxes); the
        # press is a keypress rather than a click because a tui click awaits the
        # gesture's work, which here is parked in the in-gap wait — the same
        # reason `test_connection_gap_rules.py` walks its gap by keyboard.
        app.driver.click("folder-follow-button")
        app.driver.wait_for("folder-follow-name-input", timeout=_UI_WINDOW_S)
        app.driver.clear_and_type("recipient-picker-input", owner["actor_id_hex"])
        app.driver.clear_and_type("folder-follow-name-input", other)
        app.driver.press_key("folder-follow-confirm", "Enter")
        # Wait for the FOLLOW's answer, not the first message the page shows: the
        # page has one error surface, and a refresh already in flight when the
        # nest went down fails onto it too ("Failed to load devices: …" on linux),
        # possibly before the follow has waited out its own in-gap deadline.
        failed_prefix = S.devices.error_follow_failed(message="")

        def _follow_answer():
            text = app.error_text()
            if text == S.devices.error_follow_not_found or (
                text and text.startswith(failed_prefix)
            ):
                return text
            return None

        message = wait_until(
            _follow_answer,
            _GAP_WINDOW_S,
            interval=1.0,
            diagnose=lambda: "the follow never answered; " + _followed_rows_diagnosis(app),
        )
        assert message != S.devices.error_follow_not_found, (
            "a follow that could not reach the nest reported the folder as not "
            f"existing: {message!r}"
        )

        # ── 2. The existing follow through a refresh that cannot read anything. ──
        # The revisit returns only once a refresh begun after it has committed —
        # here, after every read's in-gap wait — so what is read next is that
        # refresh's own answer, never the pre-visit snapshot.
        _revisit_folders(app, timeout=_GAP_WINDOW_S)
        assert _row_of(app, followed) is not None, (
            "a refresh that could not read the follows dropped the followed row — "
            f"a network fault read as the follow being taken away: "
            f"{_followed_rows_diagnosis(app)}"
        )
        assert _status_of(app, followed) == S.devices.followed_status_following, (
            "a network fault must never render as a revoked follow: "
            f"{_followed_rows_diagnosis(app)}"
        )
    finally:
        if nest_down:
            start_nest_in_place(nest_instance)

    # The address was good all along — the folder the gap refused is served to a
    # stranger once the nest is back — so the answer in the gap was about the gap,
    # not the folder. A read, not a second UI follow: how soon an app's follow
    # works again after an outage is its transport's redial, not this outcome
    # (web's Folders machine rides its own connection, which redials on its own
    # backoff after the app's main one reports online — `transport.md`
    # § Implementation status).
    assert _publicly_served(nest_instance, owner, other), (
        "the folder the gap refused must be publicly served once the nest is back"
    )


# ── follow-a-public-folder outcome 9 ────────────────────────────────────────


@pytest.mark.feature("follow-a-public-folder")
def test_a_followed_row_says_whose_folder_it_is(follower_app, nest_instance, cross_nest_foreign):
    """*name + owner handle + Public badge + status* (``ui/folders.md``
    § Following a public folder): followed by handle, the row names the owner by
    that handle; followed by actor id — which no kind maps back to a handle — it
    names them by the id's short form; followed by ``handle@domain`` on ANOTHER
    nest, the row names the owner by exactly that address — the handle the
    follow verified against the owner's nest, kept unchecked by design (the
    re-ratified last sentence of that paragraph). All three ride the row's own
    text, the one read every app offers.

    The cross-nest arm's owner lives on ``cross_nest_foreign`` (its own loopback
    authority as handle domain, TLS) and the follower's nest relays every read —
    the same two-nest topology the outcome-10 witness uses.
    """
    from common.auth import register_handled_actor

    app = follower_app
    owner = _owner(nest_instance)
    by_handle = _folder_for(nest_instance, owner)
    by_id = _folder_for(nest_instance, owner)
    home = cross_nest_foreign
    remote_owner = register_handled_actor(
        home["port"],
        handle="xnown" + secrets.token_hex(3),
        domain=home["authority"],
        base_url=home["url"],
    )
    on_other_nest = _folder_for(home, remote_owner, prefix="xn")
    remote_address = f"{remote_owner['handle']}@{home['authority']}"

    _follow(app, owner["handle"], by_handle)
    _follow(app, owner["actor_id_hex"], by_id)
    _follow(app, remote_address, on_other_nest)

    handle_row = _row_texts(app)[_row_of(app, by_handle)]
    assert S.devices.followed_owner(owner=owner["handle"]) in handle_row, (
        f"a follow made by handle names its owner by it: {handle_row!r}"
    )
    # The canonical short id: 12 characters and one ellipsis
    # (`behavior/value-formatting.md` § Short id; `fauna_core::format::short_id`).
    short = owner["actor_id_hex"][:12] + "\N{HORIZONTAL ELLIPSIS}"
    id_row = _row_texts(app)[_row_of(app, by_id)]
    assert S.devices.followed_owner(owner=short) in id_row, (
        f"a follow made by actor id names its owner by the id's short form: {id_row!r}"
    )
    remote_row = _row_texts(app)[_row_of(app, on_other_nest)]
    assert S.devices.followed_owner(owner=remote_address) in remote_row, (
        "a follow of a folder on another nest names its owner by the "
        f"handle@domain it was followed by: {remote_row!r}"
    )
