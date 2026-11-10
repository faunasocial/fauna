"""Tier_3 real-wire in-place MLS membership add, driven through the app UI.

**Why this module exists, separately from ``test_thread_membership.py``.**

``test_fauna_mls_real_roundtrip.py`` already proves the in-place add end-to-end
over the real backend — but it drives it through the ``conversations_real_add``
**test-agent command**. Under ``docs/goal/architecture/e2e-conventions.md``
convention 8 that is an "API-only mutation path", permitted *only* when a
client-UI-only equivalent test proves a real user reaches the same state through
the UI. **This module is that equivalent proof**, and the roundtrip's step 3 cites
it. Without it the mutation had no UI witness on any app: the mechanism was
covered, the user's route to it was not.

**Why the snapshot-helper version could never work** (the red this replaces,
``test_thread_membership.py::test_add_to_mls_group_stays_in_thread``): it built its
group with ``conversations_create_mls_group`` — whose own docstring says it
bypasses welcome / key-package distribution, and which stamps every participant
``ActorId([0u8; 32])`` — and then added a never-registered ``carol@self-nest.test``.
That address cannot become a ``Fauna`` one: ``try_parse_typed_address``
(``libs/fauna-conversations/src/address.rs:125``) has no Fauna branch by design —
its doc comment records that the variant needs an ``ActorId`` "which only a real
fauna_mls backend probe can supply" — so anything with an ``@`` parses as
``Email``. ``FaunaMlsBackend`` then refuses the membership change
(``backends/fauna_mls.rs::fauna_actor``) and ``confirm_add_participant`` rolls the
optimistic add back, leaving the count one short. The app was right and the test
was asking for something no user could do. It stayed invisible until
``logged_in_app`` began landing a real session, which overwrites the mock rail
backends — ``MockRailBackend::add_participant`` returns ``Ok(())``, so the type
mismatch used to be swallowed. ⚠ On an app still running the mock (macOS/iOS take
the real backend only behind the launch-time ``real_conversations`` gate) that old
test **passed vacuously** — it proved nothing there.

**The shape here.** Bootstrapping a real bound MLS group is *setup*, which
convention 8 carve-out (b) puts outside the drive-through-the-UI rule ("arranging
the world ≠ the action under test"), so it uses the real-wire commands. The
**action under test** — the in-place add — is driven only through the real
controls: ``thread-add-participant-button`` → ``recipient-picker-input`` →
the async resolve → the committed chip → ``add-participant-confirm``.

The peer is named by the **bare handle a user would actually type**.
``FaunaMlsBackend::resolve_address`` Form 2 (``fauna_mls.rs:1291``) looks a bare
localpart up via ``fauna.actor.by_handle`` and canonicalises it to
``localpart@domain``; ``resolve_recipient`` stashes the resolved address and
``accept_current_recipient_chip`` commits *that*, not the format-only parse. The
peer must publish real key packages first — ``resolve_reachable``
(``fauna_mls.rs:950``) resolves only when ``keypackage_count > 0``.

Assertions come in two halves on purpose. The client half (count, chips, no error)
would also be satisfied by a snapshot-only add that never touched MLS; the
nest-side half (key package consumed, Welcome delivered, Commit envelope posted)
is what proves the real membership change happened. Neither alone is honest.
"""

from __future__ import annotations

import time

import pytest

from common import create_actor_and_register
from helpers.waiting import wait_until
from i18n.strings import S
from tests.api import conv_api
from tests.api.conv_api import inbox as _inbox
from tests.api.conv_api import mint_key_packages as _mint_keypackages
from tests.test_fauna_mls_cross_device_sync import _wait_thread_snippet

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


def _thread_by_id(app, thread_id: str):
    """The thread with ``thread_id``, or None.

    Every pick in this module is by identity. alice is session-scoped and
    accumulates threads from every earlier test in the run (and from the
    cross-device replica restore), so "the last MlsGroup" reads back whichever
    one happens to sort last — a sibling test's group, not ours. That trap is
    documented across this suite; it is why `_wait_thread_snippet` exists.
    """
    for t in app.conversations.list_threads():
        if t.thread_id == thread_id:
            return t
    return None


def _wait_participant_count(app, thread_id: str, want: int, timeout: float = 20.0):
    """Poll until ``thread_id`` reports ``want`` participants; return the last
    summary seen either way.

    A deadline poll, not a settle-sleep (convention 14): the confirm click is
    fire-and-forget on native bridges and the in-place add posts a real MLS
    Commit before the snapshot observer re-renders, so the budget must cover a
    real nest round-trip. A green run pays only what it actually needs.
    """
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        for t in app.conversations.list_threads():
            if t.thread_id == thread_id:
                last = t
                if t.participant_count == want:
                    return t
        time.sleep(0.5)
    return last


def _wait_participant_actor_ids(app, thread_id: str, timeout: float = 20.0):
    """Poll until ``thread_id`` reports a non-empty ``participant_actor_ids``;
    return the last summary seen either way.

    A deadline poll, not a one-shot read straight after ``open_thread_by_id``
    (convention 14): ``participant_actor_ids`` comes from
    ``ConversationsManager::thread_detail`` (state_json.rs), which populates
    from the real MLS group's actual protocol/roster state, not from
    ``select_thread`` (a local pointer flip with no fetch of its own) — it can
    lag a moment behind both the open click and even this same thread's
    snapshot-level ``participant_count``, which comes from a different data
    path entirely.
    """
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = _thread_by_id(app, thread_id)
        if last is not None and last.participant_actor_ids:
            return last
        time.sleep(0.5)
    return last


@pytest.mark.feature("group-conversations")
def test_in_place_mls_add_through_the_ui(real_faunamls_app, nest_instance, test_user):
    """A user adds a participant to a real MLS group through the picker.

    The thread must not fork, the new member must render, and the nest must show
    the real membership change (key package consumed + Welcome + Commit).
    """
    app = real_faunamls_app
    port = nest_instance["port"]
    base = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # API-tier peers. bob is welcomed twice (the 1:1, then the forked group), so
    # he needs the deeper pool; dave is the one added through the UI.
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    carol = create_actor_and_register(port, admin_signing_key=admin_sk)
    dave = create_actor_and_register(port, admin_signing_key=admin_sk)
    for peer in (bob, carol, dave):
        conv_api.keypackage_upload(port, peer, _mint_keypackages(bytes(peer["signing_key"]), 3))
        assert conv_api.keypackage_count(port, peer, peer["actor_id_hex"]) == 3
        # Reachability is a PRECONDITION of any Welcome, the in-place add's
        # included. Under the default `allow_knock` inbox mode a non-contact's
        # Dm/Group Welcome is refused outright (`welcome_deliver_core`'s reach
        # floor → `dm_initiation_mode_verdict(None, AllowKnock)` = Knock →
        # `forbidden`; direct-messages.md § Reach policy). So a stranger cannot be
        # added to a group — correct, ratified behavior, and the reason this test
        # arranges the contact edge instead of asserting around it. `accept_contact`
        # is per-pair, so it never mutates the peer's own global mode.
        conv_api.accept_contact(port, peer, test_user["actor_id_hex"])
    # `create_actor_and_register` admits under `e2e-<12 hex>`; a bare handle
    # classifies same-nest (`peer_domain_for` → None) on every app regardless of
    # how the harness derives its own domain, which is this test's actual intent.
    dave_handle = dave["handle"]
    assert dave_handle, "the UI-added peer must have a handle to type into the picker"

    # ── setup (convention 8 carve-out (b)) — a REAL bound MLS group ──────────
    # Not the action under test: these arrange a group the real backend can
    # commit against, which `conversations_create_mls_group` cannot produce (it
    # bypasses welcome/key-package distribution and stamps ActorId([0u8; 32])).
    app.conversations.real_resolve_send_new(bob["actor_id_hex"], "hi bob")
    one = _wait_thread_snippet(app, "hi bob", timeout=20.0)
    assert one is not None, "the 1:1 with bob should surface its own echo"

    # Pin the forked group by DIFF against the ids present before the add — see
    # `_thread_by_id` on why "the last MlsGroup" is not our group.
    before_ids = {t.thread_id for t in app.conversations.list_threads()}
    app.conversations.real_add(one.thread_id, carol["actor_id_hex"], "carol")
    fresh = [
        t
        for t in app.conversations.list_threads()
        if t.thread_id not in before_ids and t.flavor == "MlsGroup"
    ]
    assert len(fresh) == 1, (
        f"adding carol to the 1:1 should fork exactly one group; got {len(fresh)}"
    )
    group_id = fresh[0].thread_id
    app.conversations.real_send(group_id, "hi group")

    group = _thread_by_id(app, group_id)
    assert group is not None, "the forked group should still be in the snapshot"
    channel = group.channel_id_hex
    assert channel, "the group must be bound to a channel before the in-place add"
    # Assert the DELTA, not an absolute count. `participant_count` is
    # `ThreadDetail.participants`, which on the real-wire path holds the peers
    # only — self is not a row — so this group reads 2 ([bob, carol]). The
    # predecessor test's absolute `== 4` came from `create_mls_group` explicitly
    # prepending `me@self-nest.test` to its member list, a property of that mock
    # helper and not of a real group. A delta is also what the assertion actually
    # means: one named person joined.
    before_count = group.participant_count
    assert before_count >= 2, (
        f"setup should leave at least [bob, carol] as peers; got {before_count}"
    )
    envelopes_before = len(conv_api.channel_fetch(port, bob, channel, after=0))
    threads_before = len(app.conversations.list_threads())

    # ── the action under test — driven ONLY through the UI ───────────────────
    # thread-add-participant-button → recipient-picker-input → the async resolve
    # (`fauna.actor.by_handle` + the keypackage reachability probe promotes the
    # handle to a `Fauna` chip) → add-participant-confirm.
    app.conversations.open_thread_by_id(group_id)
    app.conversations.add_participant_to_thread(dave_handle)

    # ── client half ─────────────────────────────────────────────────────────
    want = before_count + 1
    after = _wait_participant_count(app, group_id, want)
    assert len(app.conversations.list_threads()) == threads_before, (
        "an in-place add must not fork a new thread"
    )
    # Read the error surface BEFORE the count assert (convention 6): a refused
    # membership op ROLLS THE OPTIMISTIC ADD BACK, so a rolled-back add and an
    # add that never applied are otherwise the same bare off-by-one — which is
    # what made the predecessor red take three triage passes.
    assert after is not None and after.participant_count == want, (
        f"dave must be added in place (rail backend op + snapshot); "
        f"wanted {want}, got {after.participant_count if after else None}, "
        f"error={app.error_text()!r}"
    )
    assert app.driver.count("thread-member-chip") >= want, (
        f"the new member chip must render (>={want} after adding dave): "
        f"{app.driver.diagnose('thread-member-chip')}"
    )

    # ── nest half — proof it was a REAL membership change, not a snapshot edit ─
    assert conv_api.keypackage_count(port, dave, dave["actor_id_hex"]) == 2, (
        "dave's key package should be consumed by the in-place add"
    )
    assert len(_inbox(base, dave)) >= 1, "a Welcome should be delivered to dave"
    assert len(conv_api.channel_fetch(port, bob, channel, after=0)) > envelopes_before, (
        "the add should post a Commit envelope on the group's channel"
    )


@pytest.mark.feature("group-conversations")
def test_in_place_add_of_an_unreachable_person_is_refused(
    real_faunamls_app, nest_instance, test_user
):
    """Someone who cannot be reached yet cannot be added to a group: the add is
    refused, the person does not stay in the list, and the page says why
    (`conversations.md` § Participants vs. reply recipients).

    The inverse of :func:`test_in_place_mls_add_through_the_ui`, and the
    arrangement that test deliberately avoids. dave publishes key packages, so
    the picker resolves him to a real `Fauna` chip exactly as it would a
    reachable person; what he lacks is the CONTACT EDGE toward the adder. Under
    the default `allow_knock` inbox mode the nest refuses a non-contact's group
    Welcome (`direct-messages.md` § Reach policy), `confirm_add_participant`
    rolls the optimistic add back and stamps the page error.

    The three halves of the sentence are asserted separately, each against its
    own observable: the page's `error-message` carries the add-participant
    refusal (the barrier that the confirm has run to its end — a rollback can
    only be read after it); the group's roster is unchanged and never names dave;
    and the nest delivered dave no Welcome, which is what makes "refused" a fact
    about the wire rather than about the UI.
    """
    app = real_faunamls_app
    port = nest_instance["port"]
    base = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # bob and carol build the group, so they get the contact edge the add test
    # arranges. dave gets key packages and NOTHING ELSE — he is resolvable but
    # not reachable, which is the precondition under test.
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    carol = create_actor_and_register(port, admin_signing_key=admin_sk)
    dave = create_actor_and_register(port, admin_signing_key=admin_sk)
    for peer in (bob, carol, dave):
        conv_api.keypackage_upload(port, peer, _mint_keypackages(bytes(peer["signing_key"]), 3))
    for peer in (bob, carol):
        conv_api.accept_contact(port, peer, test_user["actor_id_hex"])
    dave_handle = dave["handle"]
    assert dave_handle, "the UI-added peer must have a handle to type into the picker"

    # ── setup (convention 8 carve-out (b)) — a REAL bound MLS group ──────────
    app.conversations.real_resolve_send_new(bob["actor_id_hex"], "hi bob unreach")
    one = _wait_thread_snippet(app, "hi bob unreach", timeout=20.0)
    assert one is not None, "the 1:1 with bob should surface its own echo"

    before_ids = {t.thread_id for t in app.conversations.list_threads()}
    app.conversations.real_add(one.thread_id, carol["actor_id_hex"], "carol")
    fresh = [
        t
        for t in app.conversations.list_threads()
        if t.thread_id not in before_ids and t.flavor == "MlsGroup"
    ]
    assert len(fresh) == 1, (
        f"adding carol to the 1:1 should fork exactly one group; got {len(fresh)}"
    )
    group_id = fresh[0].thread_id
    app.conversations.real_send(group_id, "hi group unreach")

    group = _thread_by_id(app, group_id)
    assert group is not None and group.channel_id_hex, (
        "the group must be bound to a channel before the in-place add"
    )
    before_count = group.participant_count
    assert before_count >= 2, (
        f"setup should leave at least [bob, carol] as peers; got {before_count}"
    )
    threads_before = len(app.conversations.list_threads())

    # ── the action under test — driven ONLY through the UI ───────────────────
    app.conversations.open_thread_by_id(group_id)
    app.conversations.add_participant_to_thread(dave_handle)

    # ── the app says why ────────────────────────────────────────────────────
    # The confirm is async (the wire op runs after the overlay closes), and the
    # rollback + page error land at its very end, so the error IS the barrier:
    # nothing about the roster below can be read before it without racing the
    # optimistic add.
    prefix = S.conversations.unified.error_add_participant(message="")
    refusal = wait_until(
        lambda: prefix in (app.error_text() or "") and app.error_text(),
        30.0,
        diagnose=lambda: (
            f"no add-participant refusal ever reached error-message "
            f"(error={app.error_text()!r}); the roster now reads "
            f"{getattr(_thread_by_id(app, group_id), 'participant_count', None)} "
            f"(was {before_count}) and dave's inbox holds "
            f"{len(_inbox(base, dave))} Welcome(s) — a count one higher with a "
            "Welcome means the nest ADMITTED a non-contact"
        ),
    )
    reason = refusal.split(prefix, 1)[1].strip()
    # "Why" is the reach refusal's own sentence (on tui, 2026-09-22: "This
    # person isn't accepting new conversations right now. You can send them a
    # contact request instead."), never the catch-all a failure with no
    # classified cause falls back to.
    assert reason and reason not in (S.error.send.generic, S.error.send.not_supported), (
        f"the refusal must say WHY, not only that it failed; got {refusal!r}"
    )

    # ── the person does not stay in the list ─────────────────────────────────
    after = _wait_participant_actor_ids(app, group_id)
    assert after is not None, "the group should still be in the snapshot"
    assert after.participant_count == before_count, (
        f"the refused add must be rolled back: the group had {before_count} "
        f"peers and now reports {after.participant_count}; error={refusal!r}"
    )
    assert dave["actor_id_hex"] not in (after.participant_actor_ids or []), (
        f"dave must not remain on the group's roster: {after.participant_actor_ids!r}"
    )
    assert len(app.conversations.list_threads()) == threads_before, (
        "a refused in-place add must not fork a new thread"
    )

    # ── refused on the wire, not only in the UI ──────────────────────────────
    assert _inbox(base, dave) == [], (
        "the nest must not have delivered dave a Welcome — the reach floor is "
        "what refuses the add, and a delivered Welcome would mean the rollback "
        "hid a membership change that actually happened"
    )


@pytest.mark.feature("group-conversations")
def test_in_place_mls_remove_through_the_ui(real_faunamls_app, nest_instance, test_user):
    """A user removes a participant from a real MLS group by tapping their chip.

    The removal twin of the add above, and it exists for the same convention-8
    reason: `test_fauna_mls_real_roundtrip.py` drives removal through the
    `conversations_real_remove` **test-agent command**, which is an API-only
    mutation path permitted only while a client-UI-only equivalent proves a real
    user reaches the same state. Until this test there was none on ANY app — the
    mechanism was covered, the user's route to it was not.

    **The chip IS the affordance, on every app.** There is no separate remove
    button to press: `conversations.md` § the `thread-member-chip[i]` row makes
    the chip itself the removal gesture "(when supported)", and
    `succession-aftermath.md` § Propagation → *MLS groups* leans on that — the
    review pair's *Remove* half is deliberately not re-rendered beside Keep,
    because the chip the Keep button sits inside already is it. So this test is
    also the standing witness that that half is reachable at all.

    The thread must not fork, the member must stop rendering, and the nest must
    show a real Commit — a removal posts one with **no** Welcome (nobody is
    being invited), which is what distinguishes it from the add above.
    """
    app = real_faunamls_app
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    carol = create_actor_and_register(port, admin_signing_key=admin_sk)
    for peer in (bob, carol):
        conv_api.keypackage_upload(port, peer, _mint_keypackages(bytes(peer["signing_key"]), 3))
        # Same reach precondition the add test arranges, and for the same reason
        # (`direct-messages.md` § Reach policy): a stranger cannot be welcomed
        # into a group, so the group could not be built to remove from.
        conv_api.accept_contact(port, peer, test_user["actor_id_hex"])

    # ── setup (convention 8 carve-out (b)) — a REAL bound MLS group ──────────
    app.conversations.real_resolve_send_new(bob["actor_id_hex"], "hi bob rm")
    one = _wait_thread_snippet(app, "hi bob rm", timeout=20.0)
    assert one is not None, "the 1:1 with bob should surface its own echo"

    before_ids = {t.thread_id for t in app.conversations.list_threads()}
    app.conversations.real_add(one.thread_id, carol["actor_id_hex"], "carol")
    fresh = [
        t
        for t in app.conversations.list_threads()
        if t.thread_id not in before_ids and t.flavor == "MlsGroup"
    ]
    assert len(fresh) == 1, (
        f"adding carol to the 1:1 should fork exactly one group; got {len(fresh)}"
    )
    group_id = fresh[0].thread_id
    app.conversations.real_send(group_id, "hi group rm")

    group = _thread_by_id(app, group_id)
    assert group is not None, "the forked group should still be in the snapshot"
    channel = group.channel_id_hex
    assert channel, "the group must be bound to a channel before the removal"
    before_count = group.participant_count
    assert before_count >= 2, (
        f"setup should leave at least [bob, carol] as peers; got {before_count}"
    )

    app.conversations.open_thread_by_id(group_id)
    # Which chip is carol's, by IDENTITY — `participant_actor_ids` is
    # index-parallel with the chip order, and the chip's own text is a contact
    # display name two members could share. An app whose serializer omits the
    # ids ([]) cannot name the chip it means, and removing a guessed one would
    # be a destructive assertion about the wrong person.
    #
    # Poll, don't read once right after opening: `participant_actor_ids`
    # comes from `ConversationsManager::thread_detail` (state_json.rs), which
    # populates from the real MLS group's actual protocol/roster state, not
    # from `select_thread` (a local pointer flip with no fetch of its own) —
    # it can lag a moment behind both the open click and this same thread's
    # own snapshot-level `participant_count` (a different data path).
    group = _wait_participant_actor_ids(app, group_id)
    assert group is not None, "the group should still be in the snapshot post-open"
    actor_ids = group.participant_actor_ids
    assert actor_ids, (
        "this app's state serializer exposes no participant_actor_ids after "
        "opening the thread, so the chip carrying carol cannot be identified "
        "by identity"
    )
    carol_index = next(
        (i for i, aid in enumerate(actor_ids) if aid == carol["actor_id_hex"]), None
    )
    assert carol_index is not None, (
        f"carol should hold a chip on the group; ids={actor_ids!r}"
    )

    envelopes_before = len(conv_api.channel_fetch(port, bob, channel, after=0))
    threads_before = len(app.conversations.list_threads())

    # ── the action under test — driven ONLY through the UI ───────────────────
    app.conversations.remove_member(carol_index)

    # ── client half ─────────────────────────────────────────────────────────
    want = before_count - 1
    after = _wait_participant_count(app, group_id, want)
    assert len(app.conversations.list_threads()) == threads_before, (
        "an in-place removal must not fork a new thread"
    )
    # Error surface BEFORE the count assert (convention 6): a refused membership
    # op puts the member BACK (`conversations.md` § the element table: "a failure
    # puts the member back on the list and surfaces on error-message"), so a
    # rolled-back removal and one that never applied read as the same off-by-one.
    assert after is not None and after.participant_count == want, (
        f"carol must be removed in place (rail backend op + snapshot); "
        f"wanted {want}, got {after.participant_count if after else None}, "
        f"error={app.error_text()!r}"
    )
    assert carol["actor_id_hex"] not in (after.participant_actor_ids or []), (
        "carol's chip must stop rendering — a count that merely dropped could "
        f"have dropped the wrong member: {after.participant_actor_ids!r}"
    )

    # ── nest half — proof it was a REAL membership change, not a snapshot edit ─
    # A removal re-keys the group so the removed member cannot follow forward.
    # The Commit is the observable; there is deliberately NO Welcome assertion
    # here (its absence is the point — nobody was invited).
    assert len(conv_api.channel_fetch(port, bob, channel, after=0)) > envelopes_before, (
        "the removal should post a Commit envelope on the group's channel"
    )


@pytest.mark.feature("group-conversations")
def test_re_adding_a_member_whose_welcome_never_arrived_heals_and_a_duplicate_add_changes_nothing(
    real_faunamls_app, nest_instance, test_user
):
    """Adding someone again whose earlier join never arrived brings them in,
    and re-adding a member who is already in changes nothing
    (`mls-group-key-material.md` § M2 *Admitting a member*, the chat bullet;
    the heal itself is `FaunaMlsBackend::add_participant`).

    **How the join gets lost, with no seam.** An in-place add stages and posts
    its Add commit first and delivers the newcomer's Welcome second, so a
    Welcome the nest refuses leaves the newcomer seated in the group's MLS
    state with no way to join: the *phantom leaf* the heal exists for, the same
    state a crash between the two steps leaves. dave publishes key packages but
    has no contact edge toward the adder, so under the default `allow_knock`
    inbox mode the nest refuses his Welcome (`direct-messages.md` § Reach
    policy) — the arrangement :func:`test_in_place_add_of_an_unreachable_person_is_refused`
    pins as a refusal. The UI rolls its optimistic chip back; the MLS leaf
    stays. Once dave accepts the adder, the second add through the same
    controls must evict the ghost leaf and admit him fresh. Before the heal
    landed that second add failed on MLS's duplicate-signature-key check.

    **What separates a heal from a no-op**, both read off the nest, never the
    UI alone: the heal consumes a second key package, delivers dave a Welcome
    and puts him on the channel's routing roster (`channel.actors`); the
    duplicate add that follows consumes nothing, posts no Commit and delivers
    no second Welcome. The roster is read as bob, never fetched as dave —
    `channel.fetch` auto-registers its caller, which would make the phantom
    look healthy (`conv_api.channel_actors`).
    """
    app = real_faunamls_app
    port = nest_instance["port"]
    base = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    alice_hex = test_user["actor_id_hex"]

    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    carol = create_actor_and_register(port, admin_signing_key=admin_sk)
    dave = create_actor_and_register(port, admin_signing_key=admin_sk)
    # erin is reachable from the start: the barrier add of step 3.
    erin = create_actor_and_register(port, admin_signing_key=admin_sk)
    for peer in (bob, carol, dave, erin):
        conv_api.keypackage_upload(port, peer, _mint_keypackages(bytes(peer["signing_key"]), 3))
    for peer in (bob, carol, erin):
        conv_api.accept_contact(port, peer, alice_hex)
    assert erin["handle"], "the barrier peer must have a handle to type into the picker"
    dave_hex = dave["actor_id_hex"].lower()
    dave_handle = dave["handle"]
    assert dave_handle, "the UI-added peer must have a handle to type into the picker"

    # ── setup (convention 8 carve-out (b)) — a REAL bound MLS group ──────────
    app.conversations.real_resolve_send_new(bob["actor_id_hex"], "hi bob heal")
    one = _wait_thread_snippet(app, "hi bob heal", timeout=20.0)
    assert one is not None, "the 1:1 with bob should surface its own echo"

    before_ids = {t.thread_id for t in app.conversations.list_threads()}
    app.conversations.real_add(one.thread_id, carol["actor_id_hex"], "carol")
    fresh = [
        t
        for t in app.conversations.list_threads()
        if t.thread_id not in before_ids and t.flavor == "MlsGroup"
    ]
    assert len(fresh) == 1, (
        f"adding carol to the 1:1 should fork exactly one group; got {len(fresh)}"
    )
    group_id = fresh[0].thread_id
    app.conversations.real_send(group_id, "hi group heal")

    group = _thread_by_id(app, group_id)
    assert group is not None and group.channel_id_hex, (
        "the group must be bound to a channel before the in-place add"
    )
    channel = group.channel_id_hex
    before_count = group.participant_count
    assert before_count >= 2, (
        f"setup should leave at least [bob, carol] as peers; got {before_count}"
    )
    threads_before = len(app.conversations.list_threads())

    def envelopes() -> int:
        return len(conv_api.channel_fetch(port, bob, channel, after=0))

    prefix = S.conversations.unified.error_add_participant(message="")

    # ── 1. the join that never arrives ───────────────────────────────────────
    envelopes_before_lost = envelopes()
    app.conversations.open_thread_by_id(group_id)
    app.conversations.add_participant_to_thread(dave_handle)
    wait_until(
        lambda: prefix in (app.error_text() or ""),
        30.0,
        diagnose=lambda: (
            f"the first add of dave was expected to be refused at his Welcome "
            f"(no contact edge yet), but no add-participant error appeared "
            f"(error={app.error_text()!r}); dave's inbox holds "
            f"{len(_inbox(base, dave))} Welcome(s)"
        ),
    )
    assert _inbox(base, dave) == [], "the refused add must not have delivered a Welcome"
    assert conv_api.keypackage_count(port, dave, dave["actor_id_hex"]) == 2, (
        "the refused add must have consumed one of dave's key packages — it staged "
        "a real Add commit before its Welcome was refused"
    )
    assert envelopes() > envelopes_before_lost, (
        "the refused add must have posted its Add commit (dave is now seated in "
        "the group's MLS state with no way to join — the lost join under test)"
    )
    assert dave_hex not in conv_api.channel_actors(port, bob, channel), (
        "dave must be OFF the routing roster: it is what tells the heal his "
        "Welcome never landed"
    )

    # ── 2. the add again brings him in ───────────────────────────────────────
    conv_api.accept_contact(port, dave, alice_hex)
    envelopes_before_heal = envelopes()
    app.conversations.open_thread_by_id(group_id)
    app.conversations.add_participant_to_thread(dave_handle)

    # The barrier is the nest's Welcome, never the list: the optimistic chip
    # lands before the wire op runs, so a count read first would pass on an
    # add the backend is about to refuse and roll back. A failed heal rolls
    # the chip back and its error names why (pre-heal: MLS's
    # duplicate-signature-key refusal), so the diagnosis reads the page.
    welcomes = wait_until(
        lambda: _inbox(base, dave),
        30.0,
        diagnose=lambda: (
            f"the re-add must deliver dave a Welcome; his inbox is empty and "
            f"the page says {app.error_text()!r}"
        ),
    )
    want = before_count + 1
    after = _wait_participant_count(app, group_id, want, timeout=30.0)
    error = app.error_text() or ""
    assert prefix not in error, f"the re-add must succeed; the page says {error!r}"
    assert after is not None and after.participant_count == want, (
        f"dave must be listed after the re-add: wanted {want}, got "
        f"{after.participant_count if after else None}, error={error!r}"
    )
    assert len(app.conversations.list_threads()) == threads_before, (
        "neither add may fork a new thread"
    )
    assert len(welcomes) == 1, f"exactly one Welcome for dave, got {len(welcomes)}"
    assert conv_api.keypackage_count(port, dave, dave["actor_id_hex"]) == 1, (
        "the heal re-admits with a FRESH key package (a Welcome cannot be "
        "re-minted for the old leaf), so a second one must be consumed"
    )
    assert dave_hex in conv_api.channel_actors(port, bob, channel), (
        "the delivered Welcome must put dave on the routing roster"
    )
    assert envelopes() >= envelopes_before_heal + 2, (
        "the heal is two commits — evicting the ghost leaf, then the fresh Add "
        f"(envelopes {envelopes_before_heal} → {envelopes()})"
    )

    # ── 3. re-adding a member who is already in changes nothing ──────────────
    envelopes_before_dup = envelopes()
    app.conversations.open_thread_by_id(group_id)
    app.conversations.add_participant_to_thread(dave_handle)
    # The barrier. A no-op leaves nothing to wait on, and reading straight
    # after the overlay closes would race the wire op (the confirm runs it
    # after the dialog goes) — a vacuous pass. So a REAL add follows on the
    # same group: every membership gesture on a channel runs under that
    # channel's one lock, in turn, so once erin's Add has landed the duplicate
    # has had its turn, and the envelope delta must be erin's commit alone.
    app.conversations.open_thread_by_id(group_id)
    app.conversations.add_participant_to_thread(erin["handle"])
    wait_until(
        lambda: _inbox(base, erin),
        30.0,
        diagnose=lambda: (
            f"the barrier add of erin never delivered her Welcome "
            f"(error={app.error_text()!r})"
        ),
    )
    want += 1
    _wait_participant_count(app, group_id, want, timeout=30.0)
    dup = _wait_participant_actor_ids(app, group_id)
    error = app.error_text() or ""
    assert prefix not in error, f"a duplicate add must not fail; the page says {error!r}"
    assert dup is not None and dup.participant_count == want, (
        f"a duplicate add must leave the list as it was ({want}, dave once, "
        f"plus the barrier's erin); got "
        f"{dup.participant_count if dup else None}"
    )
    assert (dup.participant_actor_ids or []).count(dave["actor_id_hex"]) == 1, (
        f"dave must be listed exactly once: {dup.participant_actor_ids!r}"
    )
    assert conv_api.keypackage_count(port, dave, dave["actor_id_hex"]) == 1, (
        "a duplicate add must not consume a key package"
    )
    assert len(_inbox(base, dave)) == 1, "a duplicate add must not deliver a second Welcome"
    assert envelopes() == envelopes_before_dup + 1, (
        "a duplicate add must post no Commit — re-admitting a healthy member "
        "would cut him off and re-invite him; the one new envelope is the "
        f"barrier's Add of erin (envelopes {envelopes_before_dup} → {envelopes()})"
    )


@pytest.mark.feature("group-conversations")
def test_re_adding_a_member_whose_join_cannot_be_judged_says_so_and_touches_nothing(
    real_faunamls_app, nest_instance, test_user
):
    """An add the app cannot judge — the person is already seated in the
    group, and the roster that would say whether their invitation arrived
    cannot be read — refuses and says so, never guessing
    (`mls-group-key-material.md` § M2 *Admitting a member*, the chat bullet's
    refuse-to-guess arm; `FaunaMlsBackend::add_participant`'s ``None`` arm).

    **The lost join** is made exactly as in
    :func:`test_re_adding_a_member_whose_welcome_never_arrived_heals_and_a_duplicate_add_changes_nothing`:
    with no contact edge, the nest refuses dave's Welcome after the Add commit
    was posted, which seats a phantom leaf.

    **The unreadable roster** is the nest's own refusal of the one read the
    heal discriminates on, ``fauna.conversations.channel.actors``
    (`rpc_hold_test_hook.rs` § Refusal), so the client needs no seam of its own:
    the app's adapter reads every failure of that read as "roster unreadable",
    which is the production path. In one nest nothing else reaches that state.
    dave accepts alice BEFORE the second add, so his Welcome would now be
    delivered: the roster read is the one thing standing between this add and
    the heal of the sibling test.

    **What refusing to guess means**, read off the nest: no Commit is posted,
    no key package is consumed and no Welcome is delivered. And the page says
    what to do, in the localized sentence. Before 2026-11-07 the arm returned
    a diagnostic, which the page renders as the generic sentence, so the
    instruction never reached the user.
    """
    from helpers.rpc_hold import release_rpc_hold, refuse_rpc, wait_for_refused_rpc

    app = real_faunamls_app
    port = nest_instance["port"]
    base = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    alice_hex = test_user["actor_id_hex"]
    roster_kind = "fauna.conversations.channel.actors"

    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    carol = create_actor_and_register(port, admin_signing_key=admin_sk)
    dave = create_actor_and_register(port, admin_signing_key=admin_sk)
    for peer in (bob, carol, dave):
        conv_api.keypackage_upload(port, peer, _mint_keypackages(bytes(peer["signing_key"]), 3))
    for peer in (bob, carol):
        conv_api.accept_contact(port, peer, alice_hex)
    dave_hex = dave["actor_id_hex"].lower()
    dave_handle = dave["handle"]
    assert dave_handle, "the UI-added peer must have a handle to type into the picker"

    # ── setup (convention 8 carve-out (b)) — a REAL bound MLS group ──────────
    app.conversations.real_resolve_send_new(bob["actor_id_hex"], "hi bob unjudged")
    one = _wait_thread_snippet(app, "hi bob unjudged", timeout=20.0)
    assert one is not None, "the 1:1 with bob should surface its own echo"

    before_ids = {t.thread_id for t in app.conversations.list_threads()}
    app.conversations.real_add(one.thread_id, carol["actor_id_hex"], "carol")
    fresh = [
        t
        for t in app.conversations.list_threads()
        if t.thread_id not in before_ids and t.flavor == "MlsGroup"
    ]
    assert len(fresh) == 1, (
        f"adding carol to the 1:1 should fork exactly one group; got {len(fresh)}"
    )
    group_id = fresh[0].thread_id
    app.conversations.real_send(group_id, "hi group unjudged")

    group = _thread_by_id(app, group_id)
    assert group is not None and group.channel_id_hex, (
        "the group must be bound to a channel before the in-place add"
    )
    channel = group.channel_id_hex
    before_count = group.participant_count

    def envelopes() -> int:
        return len(conv_api.channel_fetch(port, bob, channel, after=0))

    prefix = S.conversations.unified.error_add_participant(message="")
    refusal = S.conversations.unified.error_add_participant_unconfirmed

    # ── 1. the join that never arrives (the sibling test's step 1) ───────────
    envelopes_before_lost = envelopes()
    app.conversations.open_thread_by_id(group_id)
    app.conversations.add_participant_to_thread(dave_handle)
    wait_until(
        lambda: prefix in (app.error_text() or ""),
        30.0,
        diagnose=lambda: (
            f"the first add of dave was expected to be refused at his Welcome "
            f"(no contact edge yet), but no add-participant error appeared "
            f"(error={app.error_text()!r})"
        ),
    )
    assert refusal not in (app.error_text() or ""), (
        "the first add is refused at the Welcome, not by the roster arm"
    )
    assert _inbox(base, dave) == [], "the refused add must not have delivered a Welcome"
    assert envelopes() > envelopes_before_lost, (
        "the refused add must have posted its Add commit, seating dave as a phantom"
    )
    assert dave_hex not in conv_api.channel_actors(port, bob, channel), (
        "dave must be OFF the routing roster — a readable roster would heal him"
    )
    kps_before = conv_api.keypackage_count(port, dave, dave["actor_id_hex"])

    # ── 2. the add again, with the roster unreadable ─────────────────────────
    conv_api.accept_contact(port, dave, alice_hex)
    envelopes_before = envelopes()
    refuse_rpc(port, roster_kind)
    try:
        app.conversations.open_thread_by_id(group_id)
        app.conversations.add_participant_to_thread(dave_handle)
        wait_for_refused_rpc(port, roster_kind)
        # The barrier is the page's refusal: the backend returns it after the
        # roster read and before any commit could be staged, so once it shows,
        # every nest-side "nothing happened" below is final, not early.
        wait_until(
            lambda: refusal in (app.error_text() or ""),
            30.0,
            diagnose=lambda: (
                f"the add must say it cannot confirm dave's invitation; the page "
                f"says {app.error_text()!r}"
            ),
        )
    finally:
        release_rpc_hold(port, roster_kind)

    error = app.error_text() or ""
    assert error == S.conversations.unified.error_add_participant(message=refusal), (
        f"the page carries exactly the add-participant refusal: {error!r}"
    )
    assert dave_hex not in error.lower(), f"never a raw actor id on the page: {error!r}"
    assert envelopes() == envelopes_before, (
        "refusing to guess posts no Commit — neither an evict nor an Add "
        f"(envelopes {envelopes_before} → {envelopes()})"
    )
    assert conv_api.keypackage_count(port, dave, dave["actor_id_hex"]) == kps_before, (
        "refusing to guess consumes no key package"
    )
    assert _inbox(base, dave) == [], "refusing to guess delivers no Welcome"
    after = _thread_by_id(app, group_id)
    assert after is not None and after.participant_count == before_count, (
        f"the optimistic chip must roll back: wanted {before_count}, got "
        f"{after.participant_count if after else None}"
    )
