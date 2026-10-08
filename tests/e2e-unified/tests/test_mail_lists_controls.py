"""E2E journeys for what the mail-lists pages SHOW about a list and what their
sheets let a person SET on one.

Target state: docs/goal/behavior/mail-mass-mailing.md § `mail-lists` page UX →
§ Layout (the row's member count, last send and today's meter; the edit
button), § Add list sheet (description, help link, archive link, per-send cap),
§ The list as an alias row (the delete confirm spells out the member cascade),
§ `mail-list-members` page (re-subscribe; the import tally), and § Don't do
these (an off-server archive link asks once before saving); UX/IDs:
tests/e2e-unified/ui.yaml `mail-lists` + `mail-list-members` pages and their
`*-list` components.

The sibling `test_mail_lists.py` owns the pages' core lifecycle (create, open
members, add, unsubscribe, import, delete). Every mutation here is asserted on
the page AND over the same user WS-RPC surface the app writes.

tier_3: a real app driver against a real fauna-nest. The row-meter journey adds
the real MTA bridge's provisioned primary domain, because a list send (its
arrange) is signed for the list's domain; sending from the app is not built yet
(mailing-lists outcome 10), so that one send is the arrange, not the act under
test — the act under test is what the page shows about it.
"""

import secrets
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import app_name, skip_unbuilt
from i18n.strings import S

# tui leads (the lead app); the other six apps join by adding their marker once
# their run is green.
pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.macos, pytest.mark.ios]

DOMAIN = "lists-controls-e2e.test"
MEMBER_DOMAIN = "external.test"

# Apps that render `mail-list-members-import-result` (outcome 9); tui led.
_TALLY_BUILT_APPS = {"tui"}
# Apps whose sheet Submit arms on an off-server archive link (outcome 15); tui led.
_ARCHIVE_CONFIRM_BUILT_APPS = {"tui"}


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _lists(api):
    return api.call("fauna.bridges.list_account_lists", {})["lists"]


def _list_row(api, list_id):
    return next((r for r in _lists(api) if r["list_id"] == list_id), None)


def _members(api, list_id):
    return api.call(
        "fauna.bridges.list_list_members",
        {"list_id": list_id, "include_unsubscribed": True},
    )


def _seed_exact(api, domain):
    """An exact alias on `domain`, so the add-sheet's domain picker has it
    (`derive_list_domains` reads the caller's own rows)."""
    api.call(
        "fauna.bridges.create_account_alias",
        {
            "kind": "exact",
            "local_domain": domain,
            "pattern": "seed" + secrets.token_hex(3),
            "controls": {"label": ""},
        },
    )


def _create_list(api, domain, **fields):
    local_part = "news" + secrets.token_hex(3)
    payload = {"local_part": local_part, "local_domain": domain, **fields}
    return api.call("fauna.bridges.create_account_list", payload)["list_id"], local_part


def _add_member(api, list_id):
    address = f"reader-{secrets.token_hex(3)}@{MEMBER_DOMAIN}"
    api.call(
        "fauna.bridges.add_list_member",
        {"list_id": list_id, "recipient_address": address},
    )
    return address


def _reset(api):
    """Leave the session-scoped actor as found; a list delete cascades its
    members."""
    for row in _lists(api):
        try:
            api.call("fauna.bridges.delete_account_list", {"list_id": row["list_id"]})
        except Exception:
            pass
    for row in api.call("fauna.bridges.list_account_aliases", {})["aliases"]:
        try:
            api.call("fauna.bridges.delete_account_alias", {"alias_id": row["alias_id"]})
        except Exception:
            pass


def _wait(predicate, timeout=12.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.3)
    return predicate()


def _open_lists_page(app, rows):
    app.mail_lists.navigate()
    assert app.mail_lists.is_page_visible()
    assert _wait(lambda: app.mail_lists.row_count() >= rows), (
        f"the page should render {rows} list row(s); names={app.mail_lists.names()!r}; "
        f"error: {app.mail_lists.error_text()!r}"
    )


def _index_of(app, fragment):
    return next(i for i, n in enumerate(app.mail_lists.names()) if fragment in n)


@pytest.mark.feature("mailing-lists")
def test_row_shows_member_count_last_send_and_todays_meter(
    logged_in_app, mail_bridge_mta, test_user, nest_instance
):
    """Outcome 3: a list's row shows its subscribed member count, the day it
    last sent, and today's sends / recipients — each agreeing with the nest."""
    app = logged_in_app
    domain = mail_bridge_mta.domain
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        try:
            list_id, local_part = _create_list(api, domain, friendly_name="Meter")
            _add_member(api, list_id)
            _add_member(api, list_id)
            list_addr = f"{local_part}@{domain}"
            message = (
                "\r\n".join(
                    [
                        f"From: Meter <{list_addr}>",
                        f"To: {list_addr}",
                        f"Subject: Issue {secrets.token_hex(3)}",
                        f"Message-ID: <{secrets.token_hex(6)}@{domain}>",
                        "Date: Thu, 24 Sep 2026 12:00:00 +0000",
                        "MIME-Version: 1.0",
                        "Content-Type: text/plain; charset=utf-8",
                        "",
                        "An issue.",
                    ]
                )
                + "\r\n"
            ).encode()
            api.call(
                "fauna.bridges.send_list_message", {"list_id": list_id, "message": message}
            )
            row = _list_row(api, list_id)
            assert row["last_send_at"] and row["sends_today"] >= 1, row

            _open_lists_page(app, 1)
            i = _index_of(app, list_addr)
            assert app.mail_lists.row_text("mail-lists-list-item-member-count", i) == "2"
            assert app.mail_lists.row_text("mail-lists-list-item-last-send", i).strip(), (
                "a list that has sent must show when"
            )
            quota = app.mail_lists.row_text("mail-lists-list-item-quota", i)
            assert str(row["sends_today"]) in quota and str(row["recipients_today"]) in quota, (
                f"the meter should show today's sends and recipients {row!r}: {quota!r}"
            )
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_add_sheet_sets_description_links_and_per_send_cap(
    logged_in_app, test_user, nest_instance
):
    """Outcome 5: the add sheet's description, help link, archive link and
    lower per-send cap are all stored with the new list."""
    app = logged_in_app
    local_part = "digest" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, DOMAIN)
        try:
            app.mail_lists.navigate()
            assert app.mail_lists.is_page_visible()
            app.mail_lists.add_list(
                "Digest",
                local_part,
                description="A weekly digest",
                help_url=f"https://{DOMAIN}/help",
                archive_url=f"https://{DOMAIN}/archive",
                per_send_cap=25,
            )
            assert _wait(lambda: any(r["pattern"] == local_part for r in _lists(api))), (
                f"the list should reach the nest; error: {app.mail_lists.error_text()!r}"
            )
            row = next(r for r in _lists(api) if r["pattern"] == local_part)
            assert row["description"] == "A weekly digest", row
            assert row["list_help_url"] == f"https://{DOMAIN}/help", row
            assert row["list_archive_url"] == f"https://{DOMAIN}/archive", row
            assert row["recipients_per_send"] == 25, row
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_edit_sheet_changes_a_lists_details(logged_in_app, test_user, nest_instance):
    """Outcome 4: the edit sheet opens pre-populated and a save changes the
    list's name and description on the nest."""
    app = logged_in_app
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, DOMAIN)
        try:
            list_id, local_part = _create_list(
                api, DOMAIN, friendly_name="Before", description="old words"
            )
            _open_lists_page(app, 1)
            app.mail_lists.open_edit(_index_of(app, local_part))
            assert app.mail_lists.sheet_value("mail-lists-add-sheet-name-input") == "Before"
            assert (
                app.mail_lists.sheet_value("mail-lists-add-sheet-description-input")
                == "old words"
            )
            app.mail_lists.submit_edit(name="After", description="new words")
            assert _wait(lambda: (_list_row(api, list_id) or {}).get("friendly_name") == "After"), (
                f"the edit should reach the nest: {_list_row(api, list_id)!r}; "
                f"error: {app.mail_lists.error_text()!r}"
            )
            assert _list_row(api, list_id)["description"] == "new words"
            assert _wait(lambda: any(n.startswith("After") for n in app.mail_lists.names())), (
                app.mail_lists.names()
            )
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_delete_confirm_says_the_members_go_too(logged_in_app, test_user, nest_instance):
    """Outcome 6: the first Delete only asks, and the question says the members
    go with the list; nothing is deleted until the second press."""
    app = logged_in_app
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, DOMAIN)
        try:
            list_id, local_part = _create_list(api, DOMAIN, friendly_name="Doomed")
            _add_member(api, list_id)
            _open_lists_page(app, 1)
            i = _index_of(app, local_part)
            app.mail_lists.arm_delete(i)
            assert _wait(
                lambda: "member"
                in app.mail_lists.row_text("mail-lists-list-item-delete-button", i).lower()
            ), (
                "the armed delete must say the members go with the list: "
                f"{app.mail_lists.row_text('mail-lists-list-item-delete-button', i)!r}"
            )
            assert _list_row(api, list_id) is not None, "arming must delete nothing"
            app.mail_lists.driver.click("mail-lists-list-item-delete-button", i)
            assert _wait(lambda: _list_row(api, list_id) is None), (
                f"the confirmed delete should reach the nest; error: {app.mail_lists.error_text()!r}"
            )
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_resubscribe_from_the_members_page(logged_in_app, test_user, nest_instance):
    """Outcome 8: a member who unsubscribed can be re-subscribed from the
    members page, and the nest agrees."""
    app = logged_in_app
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, DOMAIN)
        try:
            list_id, local_part = _create_list(api, DOMAIN, friendly_name="Comeback")
            address = _add_member(api, list_id)
            api.call(
                "fauna.bridges.unsubscribe_list_member",
                {"list_id": list_id, "recipient_address": address},
            )
            _open_lists_page(app, 1)
            app.mail_lists.open_members(_index_of(app, local_part))
            assert app.mail_list_members.is_page_visible()
            assert _wait(lambda: app.mail_list_members.addresses() == [address]), (
                app.mail_list_members.addresses()
            )
            before = app.mail_list_members.status(0)
            app.mail_list_members.resubscribe(0)
            assert _wait(lambda: _members(api, list_id)["subscribed_count"] == 1), (
                f"the resubscribe should reach the nest: {_members(api, list_id)!r}; "
                f"error: {app.mail_lists.error_text()!r}"
            )
            assert _members(api, list_id)["members"][0].get("unsubscribed_at") is None
            assert _wait(lambda: app.mail_list_members.status(0) != before), (
                f"the row's status should change from {before!r}"
            )
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_import_tally_says_how_many_were_skipped(logged_in_app, test_user, nest_instance):
    """Outcome 9: a pasted batch with invalid lines and an address already on
    the list subscribes the good ones, and the page says how many were added,
    already there, and skipped as invalid — the nest's own counts."""
    app = logged_in_app
    if app_name(app.driver) not in _TALLY_BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="mail-list-members-import-result",
            detail="the import tally is built on tui first",
            tracked="",
        )
    tag = secrets.token_hex(3)
    fresh = [f"new1-{tag}@{MEMBER_DOMAIN}", f"new2-{tag}@{MEMBER_DOMAIN}"]
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, DOMAIN)
        try:
            list_id, local_part = _create_list(api, DOMAIN, friendly_name="Tally")
            existing = _add_member(api, list_id)
            # Blank lines are dropped before the call (nothing to report); the
            # invalid lines and the already-subscribed address are the skips.
            pasted = f"{fresh[0]}\n\nnot-an-address\n{existing}\n   \n@nope\n{fresh[1]}"
            _open_lists_page(app, 1)
            app.mail_lists.open_members(_index_of(app, local_part))
            assert app.mail_list_members.is_page_visible()
            assert _wait(lambda: app.mail_list_members.addresses() == [existing])

            app.mail_list_members.batch_import(pasted)

            assert _wait(lambda: _members(api, list_id)["subscribed_count"] == 3), (
                f"the two new addresses should reach the nest: {_members(api, list_id)!r}; "
                f"error: {app.mail_lists.error_text()!r}"
            )
            tally = app.mail_list_members.read_import_result()
            assert tally, (
                "the import tally should render; "
                f"error: {app.mail_lists.error_text()!r}"
            )
            assert "2 added" in tally, tally
            assert "1 already subscribed" in tally, tally
            assert "2 invalid" in tally, tally
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_an_off_server_archive_link_asks_once_before_saving(
    logged_in_app, test_user, nest_instance
):
    """Outcome 15: an archive link that points off your own server is published
    to every recipient, so the first Submit only asks (and saves nothing); the
    second saves it. Once saved, editing the list again does not re-ask about
    the same link."""
    app = logged_in_app
    if app_name(app.driver) not in _ARCHIVE_CONFIRM_BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="mail-lists-add-sheet-submit-button (off-server archive confirm)",
            detail="the armed Submit is built on tui first",
            tracked="",
        )
    local_part = "archived" + secrets.token_hex(3)
    off_server = "https://archive.example.org/" + local_part
    submit = "mail-lists-add-sheet-submit-button"
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, DOMAIN)
        try:
            app.mail_lists.navigate()
            assert app.mail_lists.is_page_visible()
            app.mail_lists.add_list("Archived", local_part, archive_url=off_server)

            confirm = S.mail_lists.archive_off_server_confirm
            assert _wait(lambda: app.mail_lists.sheet_value(submit) == confirm), (
                "the first Submit should arm with the off-server warning: "
                f"{app.mail_lists.sheet_value(submit)!r}; error: {app.mail_lists.error_text()!r}"
            )
            assert not any(r["pattern"] == local_part for r in _lists(api)), (
                "the armed Submit must save nothing"
            )

            app.mail_lists.driver.click(submit)
            assert _wait(lambda: any(r["pattern"] == local_part for r in _lists(api))), (
                f"the second Submit should save the list; error: {app.mail_lists.error_text()!r}"
            )
            list_id = next(r for r in _lists(api) if r["pattern"] == local_part)["list_id"]
            assert _list_row(api, list_id)["list_archive_url"] == off_server

            # Once: the stored link is not asked about again on a later edit.
            _open_lists_page(app, 1)
            app.mail_lists.open_edit(_index_of(app, local_part))
            app.mail_lists.submit_edit(name="Renamed")
            assert _wait(
                lambda: (_list_row(api, list_id) or {}).get("friendly_name") == "Renamed"
            ), (
                "re-saving the same link should save on the first press: "
                f"{_list_row(api, list_id)!r}; error: {app.mail_lists.error_text()!r}"
            )
        finally:
            _reset(api)
