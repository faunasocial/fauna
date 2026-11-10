"""tier_3 — with your calendar kept searchable on your nest, your search finds
a phrase from an event your calendar app saved, nobody else's search does, and
turning it off deletes the nest's calendar copy and only that one.

`[nest]` outcome of `docs/features/search.md` (the calendar sibling of the
mail witness `test_search_materialized_mail.py`). Owner doc:
`docs/goal/architecture/encryption-at-rest.md` § Capability tiering → *The
materialization grant* (parts (c) one view per granted kind under
`__materialized/<owner>/<kind>/`, (d) a calendar hit is `content_type`
`calendar` keyed by the hex `uid_hash`) and § Readable classes, class 4
(own-actor scopes only; revoke = keys dropped + that kind's view deleted).

Every step is the production path, app-less:

- mail is enabled by the shared Mail settings machine (``harness_enable_mail``),
  which leaves the MSEK in the owner's sealed custody, registers the owner's
  own calendar and card recipient keys, and hands back the plain credential a
  calendar app signs in with;
- both grants — calendar and card — are minted by the Nests page's own
  ``LinkedNestsMachine`` (``harness_mint_trust``), so the revoke can be seen
  to leave the other kind's view standing;
- the event is saved the way a person's calendar app saves it: a CalDAV PUT
  over HTTPS to the Go MDA, which seals it to the owner's calendar key;
- the owner's ``fauna.search.query`` finds it by a description phrase once the
  materializer's trickle has indexed it (a deadline poll, convention 14), the
  hit's ``content_id`` the hex ``uid_hash`` the MDA names the resource by;
- another member's query over the same phrase finds nothing;
- ``fauna.capabilities.revoke`` on the calendar grant deletes
  ``__materialized/<owner>/calendar/`` before it replies, the card view
  standing.

The restart, the type filter, the card delete and the version-1 area are
pinned at tier_3 in Rust (``bins/fauna-nest/tests/conformance_materialization_grant.rs``);
this test is the cross-process witness of the journey.
"""

import secrets
import time

import pytest

import fauna_ffi
from helpers.materialized_views import kind_dir, mint_kind, uid_hash_hex, wait_hits, hits

pytestmark = pytest.mark.tier_3


def _event(uid: str, phrase: str) -> str:
    return (
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fauna//e2e-materialized//EN\r\n"
        "BEGIN:VEVENT\r\n"
        f"UID:{uid}\r\nDTSTAMP:20361108T120000Z\r\n"
        "DTSTART:20361120T180000Z\r\nDTEND:20361120T200000Z\r\n"
        "SUMMARY:Harvest supper\r\n"
        f"DESCRIPTION:Bring the {phrase} for the long table.\r\n"
        "END:VEVENT\r\nEND:VCALENDAR\r\n"
    )


@pytest.mark.feature("search")
def test_your_calendar_kept_searchable_on_your_nest_is_found_by_you_alone_and_turning_it_off_deletes_that_view(
    nest_instance, mail_bridge_mda
):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import create_actor_and_register
    from helpers.caldav_client import CalDAVClient
    from helpers.mail_aliases import add_exact_alias

    url = nest_instance["url"]
    db_path = nest_instance["db_path"]
    admin_key = nest_instance["admin"]["signing_key"]
    owner = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_key)
    other = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_key)
    owner_seed = bytes(owner["signing_key"])
    owner_id, owner_hex = owner["actor_id_bytes"], owner["actor_id_hex"]

    # ── Arrange: mail on (the DAV credential), an address to sign in as. ──
    password = fauna_ffi.harness_enable_mail(url, owner_seed)
    address = add_exact_alias(url, owner["signing_key"], mail_bridge_mda.domain,
                              f"cal-{secrets.token_hex(4)}")

    # ── Act: the calendar and card "keep searchable" grants. ──
    calendar_grant = mint_kind(url, db_path, owner_seed, owner_id, "calendar")
    mint_kind(url, db_path, owner_seed, owner_id, "card")
    calendar_view = kind_dir(db_path, owner_hex, "calendar")
    card_view = kind_dir(db_path, owner_hex, "card")

    # ── Act: the calendar app saves an event. ──
    cal = CalDAVClient(f"https://127.0.0.1:{mail_bridge_mda.caldav_port}",
                       address, password, verify=False)
    cal.wait_until_serving()
    cal_href = cal.personal_calendar()
    uid = f"supper-{secrets.token_hex(6)}@e2e.test"
    phrase = f"quincelanterns{time.time_ns()}"
    cal.put_event(cal_href, uid, _event(uid, phrase))
    href = cal.href_for_uid(cal_href, uid)
    assert href, f"the saved event must be listed in its calendar: {cal.event_hrefs(cal_href)!r}"
    want_id = uid_hash_hex(href, ".ics")

    owner_ws = WsRpcAdminClient(url, actor_id=owner_id, signing_key=owner_seed)
    other_ws = WsRpcAdminClient(url, actor_id=other["actor_id_bytes"],
                                signing_key=bytes(other["signing_key"]))
    with owner_ws, other_ws:
        # ── Assert: the owner finds the phrase. ──
        found = wait_hits(owner_ws, phrase, "calendar")
        assert len(found) == 1, (
            f"the owner's search must find the saved event by a description phrase; "
            f"calendar hits: {found!r}"
        )
        assert found[0]["content_id"] == want_id, (
            f"a calendar hit's content id must be the hex uid_hash ({want_id}): {found[0]!r}")
        assert calendar_view.is_dir() and card_view.is_dir(), (
            f"each granted kind keeps its own view: {calendar_view}, {card_view}")

        # ── Assert: nobody else's search does. ──
        assert hits(other_ws, phrase, "calendar") == [], (
            "another member's search must never reach the owner's materialized calendar")

        # ── Act + assert: turning it off deletes the calendar copy alone. ──
        owner_ws.call("fauna.capabilities.revoke", {"grant_id": calendar_grant})
        assert not calendar_view.exists(), (
            f"revoke must delete the calendar view before it replies: {calendar_view}")
        assert card_view.is_dir(), (
            f"the card view rides its own grant and must stand: {card_view}")
        assert hits(owner_ws, phrase, "calendar") == [], (
            "after revoke the owner's search must no longer find the event")
