"""Add-participant: forks a new group on a FaunaMls 1:1 (snapshot-only).

The **in-place** add on a bound MLS group lives in ``test_thread_membership_real.py``,
not here, and is deliberately not reachable through this module's mock-inject
setup — see that module's docstring for why the snapshot-helper version of it could
never pass. Keeping the two apart also respects the ``real_conversations`` marker's
session-wide reach (``conftest.py``'s ``_apply_real_conversations_env``): a marked
test sharing this module would flip these mock-inject tests to the real backend too.

The fork below stays honest without a real backend because a FaunaMls 1:1 fork is
**snapshot-only** — it mints a fresh participant-keyed group and fires no rail
backend op, so a non-Fauna-typed address (an email address at a domain no nest
serves — see the inline comment on the ``add_participant_to_thread`` call) is fine
here in a way it is not for the in-place add.
"""

import pytest

pytestmark = pytest.mark.tier_3

@pytest.mark.feature("group-conversations")
def test_add_to_oneonone_forks_new_group(logged_in_app):
    thread_id = logged_in_app.conversations.inject_and_resolve_thread(
        rail="FaunaMls", sender="bob-membership@self-nest.test", body="hi"
    )
    before_ids = {t.thread_id for t in logged_in_app.conversations.list_threads()}
    logged_in_app.conversations.open_thread_by_id(thread_id)
    # An email address at a domain no nest serves (`test_recipient_picker.py::
    # test_resolves_email`'s fixture shape): it resolves by shape on the Smtp rail,
    # which is all a snapshot-only fork needs — the type is incidental, only the
    # FORK is asserted. Not a `*@self-nest.test` address: the injected thread's
    # participant harvest makes that domain `KnownFauna` (bob-membership is its
    # sender), so it probes the nest and fails here — the ruling that refused the
    # "Unloaded domain_evidence" reading of this failure, 2026-09-07. Not a bare `did:plc:…` either (the fixture until 2026-10-13): it
    # leaned on the Bluesky rail's format-only parse, retired 2026-10-03, and now
    # reads `not-found`, so no chip commits and the fork never starts.
    logged_in_app.conversations.add_participant_to_thread("alice-fork@plain-email-host.test")
    after = logged_in_app.conversations.list_threads()
    # Count only the fork this test made — a FaunaMls thread that was not there
    # before — never the whole list. The list is not otherwise still: the mail
    # rail's launch re-drain and every rail's receive poll add threads under a
    # running test (2026-09-22 whole-suite linux sweep: `assert 26 == (18 + 1)`).
    forks = [t for t in after if t.thread_id not in before_ids and t.rail == "FaunaMls"]
    assert [t.thread_id for t in forks] and len(forks) == 1, (
        "the fork must create exactly one new FaunaMls group thread; new FaunaMls "
        f"threads since the add: {[(t.thread_id, t.label) for t in forks]}"
    )
    # By identity: this session-scoped list holds every earlier test's 1:1s too, so
    # `flavor == "OneToOne"` would read back whichever one happens to sort first.
    forked_from = next(t for t in after if t.thread_id == thread_id)
    assert forked_from.participant_count == 2, "original 1:1 unchanged"

# `test_add_to_mls_group_stays_in_thread` was REPLACED 2026-08-03 by
# `test_thread_membership_real.py::test_in_place_mls_add_through_the_ui`, a strict
# superset: the same "must not fork" + participant-count + member-chip assertions,
# but against a group the real FaunaMls backend can actually commit to, plus the
# nest-side proof (key package consumed, Welcome delivered, Commit posted) that the
# membership change was real rather than a snapshot edit.
#
# Do not restore the version that stood here — it could not pass. It built its group
# with `conversations_create_mls_group` (which bypasses welcome/key-package
# distribution and stamps every participant `ActorId([0u8; 32])`) and added a
# never-registered `carol@self-nest.test`, which `try_parse_typed_address` can only
# type as `Email`; the backend refused the membership change and
# `confirm_add_participant` rolled the optimistic add back. It also passed VACUOUSLY
# on any app still running the mock backend, where `MockRailBackend::add_participant`
# returns `Ok(())` and swallows the type mismatch.
