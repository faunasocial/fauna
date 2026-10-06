"""tier_1 unit: phase 5 of the three-machine live filesync test — the delete
barrier (``_run_delete_phase``) and the applied-delete wait (``_await_deleted``).
No nest, no driver, no cohort: real files in a tmp dir, stubbed waits.

Why this exists at all: phase 5 is the round's FIRST delete phase. The round
deliberately had none, because deleting a run's files from the fastest seat
races a slower seat's phase-4 wait — the tombstone wins batch-latest, the done
file never materialises there, and that seat's wait cannot be satisfied by
anything ever again. Phase 5a's barrier is the precondition that removes the
race, so **the ordering is the safety mechanism**, not an implementation detail
of it. A live tri-machine round is this project's most expensive test (three
machines, an operator holding a cohort open, a shared live-nest folder) and
it would exercise the ordering exactly once per run, in the one direction that
happens to occur — so the mechanism is pinned here and the round is left to
prove only what needs three machines, which is that a tombstone crosses an OS
boundary at all.

What the round still owes, and this file deliberately does not fake: the
cross-OS propagation itself.
"""

import pytest

from tests import test_filesync_multiseat_live as live

pytestmark = pytest.mark.tier_1

RUN = "20260806-01"
SEAT = "macos"
PEERS = ["linux", "windows"]


@pytest.fixture
def folder(tmp_path, monkeypatch):
    """A bound-folder stand-in holding the state phase 4 leaves behind: every
    seat's done file present, because every seat wrote one and this seat's
    phase-4 wait returned."""
    monkeypatch.setattr(live, "RUN_ID", RUN)
    monkeypatch.setattr(live, "WINDOW", 1.0)
    monkeypatch.setattr(live.time, "sleep", lambda _s: None)  # no wall-clock cost
    d = tmp_path / "multiseat"
    d.mkdir()
    for s in [SEAT, *PEERS]:
        live._done_path(d, s).write_text(live._done(s, "deadbeef"))
    return d


# ── the ordering contract: 5a strictly before the unlink ─────────────────────


def test_the_unlink_is_unreachable_until_the_seen_barrier_returns(folder):
    """The whole point of phase 5a. If a seat could unlink before every peer
    said it holds the file, the round is back to the hazard that kept it from
    having a delete phase at all — and the peer it strands waits forever."""
    seen_at_barrier = {}

    def await_seen(_expect):
        seen_at_barrier["own_done_present"] = live._done_path(folder, SEAT).exists()

    live._run_delete_phase(SEAT, folder, PEERS, await_seen, lambda _paths: None)

    assert seen_at_barrier["own_done_present"] is True, (
        "the seat unlinked its own done file BEFORE the phase-5a barrier "
        "returned — a peer that has not yet materialised it can now never "
        "satisfy its phase-4 wait"
    )
    assert not live._done_path(folder, SEAT).exists(), (
        "the barrier returned but the unlink never happened, so no tombstone "
        "is recorded and every peer's phase-5b wait will time out"
    )


def test_the_seen_file_is_published_before_the_barrier_waits(folder):
    """A seat that waited before publishing would deadlock the whole cohort:
    every seat would be waiting for a file no seat has written yet."""
    published = {}

    def await_seen(_expect):
        published["mine"] = live._seen_path(folder, SEAT).read_text()

    live._run_delete_phase(SEAT, folder, PEERS, await_seen, lambda _paths: None)
    assert published["mine"] == live._seen(SEAT)


def test_the_barrier_waits_for_every_peer_and_never_for_itself(folder):
    """Peers-only, like every other wait in this test — a seat waiting on its
    own file would hang on something no peer will ever write."""
    captured = {}
    live._run_delete_phase(
        SEAT, folder, PEERS, lambda expect: captured.update(expect=expect), lambda _p: None
    )
    assert sorted(p.name for p in captured["expect"]) == [
        f"{RUN}-seen-linux.txt",
        f"{RUN}-seen-windows.txt",
    ]


def test_a_seat_deletes_only_its_OWN_done_file(folder):
    """One seat cleaning up the whole run would need permission from every peer
    for every path. Deleting only what this seat alone wrote is what keeps the
    barrier one-sided — and it is also what makes every ordered
    (recording OS -> applying OS) pair get exercised in one run."""
    live._run_delete_phase(SEAT, folder, PEERS, lambda _e: None, lambda _p: None)
    for p in PEERS:
        assert live._done_path(folder, p).exists(), (
            f"this seat removed {p}'s done file; only its author may delete it, "
            f"and only after the peers have said they hold it"
        )


def test_the_phase_5b_wait_is_handed_exactly_the_peers_done_files(folder):
    captured = {}
    live._run_delete_phase(
        SEAT, folder, PEERS, lambda _e: None, lambda paths: captured.update(paths=paths)
    )
    assert sorted(p.name for p in captured["paths"]) == [
        f"{RUN}-done-linux.txt",
        f"{RUN}-done-windows.txt",
    ]


# ── _await_deleted: the applied-delete observation ────────────────────────────


def test_it_returns_once_every_peer_delete_applied(folder):
    """The green path: both peers' files already gone. A green run pays none of
    the budget — the first poll returns."""
    for p in PEERS:
        live._done_path(folder, p).unlink()
    live._await_deleted(None, None, [live._done_path(folder, p) for p in PEERS], "phase 5b")


def test_a_file_still_on_disk_reds_and_names_it(folder, monkeypatch):
    """A tombstone that never applied here is a real defect — never a pass."""
    live._done_path(folder, "linux").unlink()  # one applied, one did not
    monkeypatch.setattr(live, "_nest_listing", lambda _app: (set(), True))
    with pytest.raises(AssertionError) as e:
        live._await_deleted(
            None, None, [live._done_path(folder, p) for p in PEERS], "phase 5b"
        )
    msg = str(e.value)
    assert f"{RUN}-done-windows.txt" in msg
    assert f"{RUN}-done-linux.txt" not in msg, (
        "the message named a file that HAD applied — it must report only what "
        "is genuinely outstanding"
    )


def test_the_failure_says_a_re_poll_will_not_help(folder, monkeypatch):
    """`file-sync.md` § *Deletes propagate the same way*: the pull anchor
    advances whether or not a change was applied, so a declined tombstone is
    excluded from every later pull. A session must not read this red as
    slowness and re-run it on a quieter machine."""
    monkeypatch.setattr(live, "_nest_listing", lambda _app: (set(), True))
    with pytest.raises(AssertionError) as e:
        live._await_deleted(
            None, None, [live._done_path(folder, p) for p in PEERS], "phase 5b"
        )
    assert "will NOT resolve on a re-poll" in str(e.value)


# ── the two-sided diagnosis: which machine is actually at fault ──────────────


def test_gone_from_the_nest_but_here_blames_this_seats_apply():
    """The tombstone provably reached the nest, so the deleter did its half."""
    d = live._still_present_diagnosis(f"{RUN}-done-linux.txt", SEAT, set(), True)
    assert "GONE FROM THE NEST" in d
    assert "nest -> macos apply is the broken direction" in d
    assert "linux is NOT the suspect" in d


def test_still_on_the_nest_blames_the_deleters_record_path():
    """The mirror image: nothing was ever recorded, so this seat's apply path
    has had nothing to apply."""
    d = live._still_present_diagnosis(
        f"{RUN}-done-linux.txt", SEAT, {f"{RUN}-done-linux.txt"}, True
    )
    assert "STILL ON THE NEST" in d
    assert "linux -> nest is the broken direction" in d
    assert "This seat's apply path is NOT the suspect" in d


def test_an_unreadable_nest_blames_NOBODY():
    """Convention 6's rider — diagnose against a witness you HOLD. With no nest
    read there is no evidence for either side, and guessing has twice sent a
    session at the wrong machine."""
    d = live._still_present_diagnosis(f"{RUN}-done-linux.txt", SEAT, None, True)
    assert "UNDETERMINED" in d
    assert "do not blame any machine" in d
    assert "broken direction" not in d


def test_a_lossy_read_does_not_harden_into_an_accusation():
    """The mirror of the lossy rule one page up, and it caught a real bug when
    first written: a read that dropped rows cannot prove the nest no longer
    lists the file, so "GONE FROM THE NEST, therefore this seat failed to
    apply" is an accusation the evidence does not support. It must stay hedged."""
    d = live._still_present_diagnosis(f"{RUN}-done-linux.txt", SEAT, set(), False)
    assert "LOSSY" in d
    assert "most likely" in d
    assert "cannot prove the nest dropped it" in d
    assert "GONE FROM THE NEST" not in d


def test_presence_in_a_lossy_read_is_still_sound():
    """The other half of the same rule: a name we DID read is really listed,
    however many rows went unregistered — so this one may accuse."""
    d = live._still_present_diagnosis(
        f"{RUN}-done-linux.txt", SEAT, {f"{RUN}-done-linux.txt"}, False
    )
    assert "STILL ON THE NEST" in d
    assert "linux -> nest is the broken direction" in d


def test_the_diagnosis_survives_a_file_with_no_single_author():
    """The shared file has no author. Nothing in phase 5 deletes it today, but
    the helper must not crash if a future leg widens the set."""
    d = live._still_present_diagnosis(f"{RUN}-shared.txt", SEAT, set(), True)
    assert "a peer" in d
