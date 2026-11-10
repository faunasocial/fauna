"""tier_3 nest witnesses of the scheduling promises the shared client code
keeps: who may change an event by an inbound iTIP message
(``docs/goal/behavior/inbound-scheduling-authority.md`` § Who may mutate an
existing event over the inbound rail) and the mailbox-less attendee's answer
over the sealed rail (``docs/goal/behavior/caldav-server.md`` § Scheduling &
invitations — the organizer's client merges the reply on its next sync).

The nest never applies an invitation or a reply: the RECIPIENT's client does,
through ``fauna_client_caldav``'s inbound apply, and the nest supplies only the
facts it checks — the record author it attests on ``channel.fetch`` and the
mail door's ``X-Fauna-Authenticated-Sender`` stamp. So these are witnessed
app-less through the e2e harness library (``feature-catalog.md`` § The two
surfaces): every event is stored and read through the shared CalDAV client,
every iMIP is built by the shared builders, crosses the real nest over the
sealed scheduling rail (or is filed as the mail door files it), and is applied
by the shared apply on the recipient's side. The Rust pin of the same flow is
``bins/fauna-nest/tests/conformance_caldav_scheduling_mailbox_less.rs``.

Addresses resolve through a directory the test hands the apply — the stand-in
for anonymous discovery, which needs DNS the harness has none of; every
authority decision on top of it is the shared apply's.
"""

from __future__ import annotations

import secrets

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3


class _Person:
    """A registered actor with its client-held calendar key and the address
    its invitations name."""

    def __init__(self, nest_instance, name: str):
        actor = create_actor_and_register(
            nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
        )
        self.secret = bytes(actor["signing_key"])
        self.actor_hex = actor["actor_id_hex"]
        self.msek = secrets.token_bytes(32)
        self.address = f"{name}-{self.actor_hex[:8]}@fauna.test"
        # Everyone here is a stranger to everyone else; `open` is the inbox
        # mode under which a stranger's invite is deliverable at all
        # (`direct-messages.md` § Reach policy). What these tests are about is
        # what the recipient's client does with it once delivered.
        with WsRpcAdminClient(
            nest_instance["url"], actor_id=actor["actor_id_bytes"], signing_key=self.secret
        ) as ws:
            ws.call("fauna.inbox.mode.set", {"mode": "open"})


def _people(nest_instance, *names: str) -> tuple[list[_Person], dict[str, str]]:
    people = [_Person(nest_instance, n) for n in names]
    return people, {p.address: p.actor_hex for p in people}


def _events(nest_instance, who: _Person) -> dict[str, dict]:
    return {
        e["uid"]: e
        for e in fauna_ffi.harness_caldav_events(nest_instance["url"], who.secret, who.msek)
    }


def _invite(nest_instance, sender, recipient, directory, **kw) -> dict:
    return fauna_ffi.harness_scheduling_invite(
        nest_instance["url"], sender.secret, recipient.secret, recipient.msek,
        directory=directory, **kw,
    )


def _rsvp(nest_instance, sender, organizer, directory, **kw) -> dict:
    return fauna_ffi.harness_scheduling_rsvp(
        nest_instance["url"], sender.secret, sender.msek, organizer.secret, organizer.msek,
        directory=directory, **kw,
    )


@pytest.mark.feature("calendar-and-events")
def test_only_the_organizer_can_rewrite_an_event_and_a_reply_counts_only_for_its_sender(
    nest_instance,
):
    """`calendar-and-events` outcome 19: only the organizer can rewrite an
    event by mail, and a mailed reply counts only for the attendee who sent
    it; anyone else's rewrite or reply is refused and leaves the event
    untouched.

    alice invites bob and mallory; both clients materialize the event. mallory
    — a co-attendee holding the UID and alice's address — sends bob a REQUEST
    byte-identical in form to alice's but retitled: bob's client refuses it
    and his event reads back byte-for-byte as before, while alice's own
    retitling REQUEST updates it. On alice's side, mallory answers *as bob*
    over the sealed rail and over mail (her door-authenticated address on the
    copy): both refused, bob still NEEDS-ACTION on alice's event. bob's own
    answer — over each rail — is applied."""
    (alice, bob, mallory), directory = _people(nest_instance, "alice", "bob", "mallory")
    uid = f"kickoff-{secrets.token_hex(6)}@fauna.test"
    roster = [bob.address, mallory.address]
    event = dict(uid=uid, organizer=alice.address, attendees=roster)

    fauna_ffi.harness_caldav_put_event(
        nest_instance["url"], alice.secret, alice.msek, summary="Project kickoff", **event
    )
    for attendee in (bob, mallory):
        got = _invite(nest_instance, alice, attendee, directory,
                      method="REQUEST", summary="Project kickoff", **event)
        assert got["outcome"] == "created", f"alice's invite to {attendee.address}: {got}"
        assert got["author"] == alice.actor_hex, f"the nest attests alice as the poster: {got}"
    before = _events(nest_instance, bob)[uid]
    assert before["summary"] == "Project kickoff"

    # mallory replays alice's REQUEST, retitled, to bob.
    forged = _invite(nest_instance, mallory, bob, directory,
                     method="REQUEST", summary="Kickoff moved to my place", **event)
    assert forged["author"] == mallory.actor_hex, f"the nest attests mallory: {forged}"
    assert (forged["outcome"], forged["reason"]) == ("refused", "not_the_organizer"), (
        f"a co-attendee's rewrite of bob's event was not refused: {forged}"
    )
    after = _events(nest_instance, bob)[uid]
    assert (after["etag"], after["ics"]) == (before["etag"], before["ics"]), (
        "bob's event changed under a refused rewrite"
    )

    # alice's own retitling REQUEST is applied.
    genuine = _invite(nest_instance, alice, bob, directory,
                      method="REQUEST", summary="Project kickoff (room 4)", **event)
    assert genuine["outcome"] == "updated", f"the organizer's own rewrite: {genuine}"
    assert _events(nest_instance, bob)[uid]["summary"] == "Project kickoff (room 4)"

    # mallory answers as bob, on each rail: refused, alice's event untouched.
    held = _events(nest_instance, alice)[uid]
    assert held["attendees"][bob.address.lower()] == "NEEDS-ACTION"
    for rail, door_sender in (("sealed", None), ("mail", mallory.address)):
        spoof = _rsvp(nest_instance, mallory, alice, directory, uid=uid,
                      as_address=bob.address, rail=rail, door_sender=door_sender)
        assert (spoof["outcome"], spoof["reason"]) == ("refused", "not_the_attendee"), (
            f"mallory's {rail} reply on bob's behalf was not refused: {spoof}"
        )
        now = _events(nest_instance, alice)[uid]
        assert (now["etag"], now["ics"]) == (held["etag"], held["ics"]), (
            f"alice's event changed under a refused {rail} reply"
        )

    # bob's own answers are applied: declined over the sealed rail, then going
    # by mail under his own door-authenticated address.
    own = _rsvp(nest_instance, bob, alice, directory, uid=uid, as_address=bob.address,
                response="declined", rail="sealed")
    assert own["outcome"] == "applied", f"bob's own sealed reply: {own}"
    assert _events(nest_instance, alice)[uid]["attendees"][bob.address.lower()] == "DECLINED"
    mailed = _rsvp(nest_instance, bob, alice, directory, uid=uid, as_address=bob.address,
                   response="going", rail="mail", door_sender=bob.address,
                   timestamp=1_700_000_200)
    assert mailed["outcome"] == "applied", f"bob's own mailed reply: {mailed}"
    assert _events(nest_instance, alice)[uid]["attendees"][bob.address.lower()] == "ACCEPTED"


@pytest.mark.feature("calendar-in-standard-apps")
def test_a_mailbox_less_attendees_answer_over_the_sealed_rail_shows_them_accepted(
    nest_instance,
):
    """`calendar-in-standard-apps` outcome 30: an attendee without a mailbox
    answers an invitation over the sealed rail, and the organizer's event
    shows them accepted.

    bob has no mailbox: nothing here enables mail for anyone. alice's
    invitation reaches his client over the sealed scheduling rail and
    materializes on his calendar; he answers going, his client sends the
    REPLY back over the same rail, and alice's client merges it — her stored
    event, read back, shows bob ACCEPTED."""
    (alice, bob), directory = _people(nest_instance, "alice", "bob")
    uid = f"standup-{secrets.token_hex(6)}@fauna.test"
    event = dict(uid=uid, organizer=alice.address, attendees=[bob.address])

    fauna_ffi.harness_caldav_put_event(
        nest_instance["url"], alice.secret, alice.msek, summary="Standup", **event
    )
    got = _invite(nest_instance, alice, bob, directory, method="REQUEST", summary="Standup",
                  **event)
    assert got["outcome"] == "created", f"the invite over the sealed rail: {got}"
    assert _events(nest_instance, bob)[uid]["summary"] == "Standup", (
        "the invitation did not land on the attendee's calendar"
    )
    assert _events(nest_instance, alice)[uid]["attendees"][bob.address.lower()] == "NEEDS-ACTION"

    answer = _rsvp(nest_instance, bob, alice, directory, uid=uid, as_address=bob.address,
                   response="going", rail="sealed")
    assert answer["author"] == bob.actor_hex, f"the nest attests bob as the replier: {answer}"
    assert answer["outcome"] == "applied", f"the organizer's merge of bob's answer: {answer}"
    stored = _events(nest_instance, alice)[uid]
    assert stored["attendees"][bob.address.lower()] == "ACCEPTED", (
        f"alice's stored event does not show bob accepted: {stored['attendees']}"
    )
