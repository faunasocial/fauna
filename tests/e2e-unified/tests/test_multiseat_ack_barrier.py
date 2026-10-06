"""tier_1 unit: the phase-4 own-ack barrier of the three-machine live filesync
test (``_await_own_ack_on_nest``). No nest, no driver, no cohort — the Media
read is stubbed, so this pins the barrier's LOGIC headlessly.

Why this exists at all: the barrier was added because run ``20260724-06`` had the
mac seat print ``cohort converged … — PASS`` while its own
``20260724-06-done-macos.txt`` never reached the nest, stranding both peers (who
block on exactly that file). The only thing that exercised the barrier was a
live tri-machine round — precisely the "the mechanism is excused from testing
because a bigger test covers it" shape the untested-mechanism rule forbids.
A live round is also the most expensive test this project has: three
machines, an operator holding a cohort open, and a shared live-nest folder.
So the mechanism is pinned here and the round is left to prove only what needs
three machines.
"""

import pytest

from actions.media import MediaActions
from helpers import multiseat_config as cfg
from tests import test_filesync_multiseat_live as live

pytestmark = pytest.mark.tier_1


@pytest.fixture
def barrier(monkeypatch):
    """Pin RUN_ID + a short WINDOW, and hand back a helper that runs the barrier
    against a scripted sequence of Media reads."""
    monkeypatch.setattr(live, "RUN_ID", "20260724-06")
    monkeypatch.setattr(live, "WINDOW", 1.0)
    monkeypatch.setattr(live.time, "sleep", lambda _s: None)  # no wall-clock cost

    def run(reads, seat="macos"):
        """`reads` is a list of per-call outcomes; the last entry repeats once
        exhausted. Each entry is one of:

        * ``[names…]``            — a COMPLETE read (every row was readable)
        * ``([names…], rows)``    — an explicit row count; ``rows > len(names)``
          is a LOSSY read (the lazy-list gap on a native GUI app)
        * an ``Exception`` instance — raised, i.e. the read did not complete

        Stubs ``_read_set_listing``, the ``(names, rows)`` primitive, and derives
        ``_read_set_item_names`` from it exactly as production does — so a test
        that does not care about completeness keeps using the bare-list form."""
        calls = {"n": 0}

        def fake_listing(_app):
            i = min(calls["n"], len(reads) - 1)
            calls["n"] += 1
            out = reads[i]
            if isinstance(out, Exception):
                raise out
            if isinstance(out, tuple):
                names, rows = out
                return list(names), rows
            return list(out), len(out)  # bare list ⇒ complete read

        monkeypatch.setattr(live, "_read_set_listing", fake_listing)
        monkeypatch.setattr(
            live, "_read_set_item_names", lambda app: fake_listing(app)[0]
        )
        live._await_own_ack_on_nest(object(), seat)
        return calls["n"]

    return run


PRESENT = [
    "20260724-06-done-linux.txt",
    "20260724-06-done-macos.txt",
    "20260724-06-done-windows.txt",
]
# The exact listing the live run produced: every file EXCEPT this seat's own ack.
WITHOUT_OWN_ACK = [
    "20260724-06-done-linux.txt",
    "20260724-06-done-windows.txt",
    "20260724-06-hello-linux.txt",
    "20260724-06-hello-macos.txt",
    "20260724-06-hello-windows.txt",
    "20260724-06-ready-linux.txt",
    "20260724-06-ready-macos.txt",
    "20260724-06-ready-windows.txt",
    "20260724-06-shared.txt",
]


def test_a_visible_own_ack_passes_on_the_first_read(barrier):
    """A green run pays exactly one Media read — the barrier must not poll when
    the ack is already there."""
    assert barrier([PRESENT]) == 1


def test_the_run_20260724_06_listing_fails_the_barrier(barrier):
    """The regression this barrier exists for, replayed from the real listing:
    nine of ten run files present, this seat's own ack absent. Before the
    barrier this seat printed PASS."""
    with pytest.raises(AssertionError) as e:
        barrier([WITHOUT_OWN_ACK])
    msg = str(e.value)
    assert "20260724-06-done-macos.txt" in msg
    # The message must send the reader at THIS machine, not at a peer — the
    # peer-only blind spot misdirected three sessions before.
    assert "THIS machine" in msg
    # ...and it must show what WAS readable, so the failure diagnoses itself
    # (convention 6: never debug this with a screenshot).
    assert "20260724-06-done-linux.txt" in msg


def test_a_late_ack_is_awaited_not_failed(barrier):
    """An upload that lands on the third read is a PASS — the barrier waits for
    state, so a slow-but-healthy drain must not read as a failure (the
    "NEVER ARRIVED really meant not yet" lesson, convention 14)."""
    assert barrier([WITHOUT_OWN_ACK, WITHOUT_OWN_ACK, PRESENT]) == 3


def test_a_failing_media_read_keeps_polling(barrier):
    """A transient list/RPC hiccup must not abort the barrier — it is the peers'
    unblock condition, so it fails only on a real deadline."""
    assert barrier([RuntimeError("transient RPC"), PRESENT]) == 2


def test_a_permanently_failing_read_fails_with_the_cause(barrier):
    """If the Media read never completes, say so — an unexplained barrier
    timeout would be misread as a sync failure."""
    with pytest.raises(AssertionError) as e:
        barrier([RuntimeError("list never loaded")])
    assert "list never loaded" in str(e.value)


def test_another_seats_ack_does_not_satisfy_this_seat(barrier):
    """The barrier is per-seat: linux's ack being up says nothing about mine."""
    with pytest.raises(AssertionError) as e:
        barrier([["20260724-06-done-linux.txt"]], seat="windows")
    assert "20260724-06-done-windows.txt" in str(e.value)


def test_a_stale_run_ids_ack_does_not_satisfy_the_barrier(barrier):
    """A previous round's ack must never satisfy this round — run ids prefix
    every basename precisely so stale files cannot pass a wait."""
    with pytest.raises(AssertionError):
        barrier([["20260724-05-done-macos.txt"]])


# --- absence is only sound from a COMPLETE read -----------------------------
#
# `_read_set_listing` tolerates a per-item lookup miss: a lazy-list GUI app
# registers only on-screen rows, so an off-screen `media-item` name is simply
# unreadable. PRESENCE survives that (a name we read is really there); ABSENCE
# does not — a missing name may just never have been registered. Run -06 did not
# expose this because the mac seat ran `--client tui`, which registers every row
# OF ITS SNAPSHOT (true — but only for this registration axis; run -07 then
# false-red-ed a tui seat on the OTHER axis, staleness — the freshness section
# below). A `--client macos`/`windows` seat would have the barrier red an ack
# that IS on the nest and merely unread. A false red here is the expensive kind:
# it fails a correct run and sends the next session hunting a sync bug that does
# not exist.


def test_a_lossy_read_does_not_red_the_barrier(barrier, capsys):
    """Nine names readable but the client believes there are twelve rows: the
    ack's absence is UNPROVEN, so the barrier must not claim the seat starved
    its peers. It degrades to a loud UNCERTIFIED warning instead."""
    barrier([(WITHOUT_OWN_ACK, 12)])
    out = capsys.readouterr().out
    assert "UNCERTIFIED" in out
    assert "20260724-06-done-macos.txt" in out


def test_a_lossy_read_still_passes_on_a_visible_ack(barrier):
    """Presence is sound on ANY client — an ack we actually read is on the nest,
    however many rows went unregistered. A lossy read must not downgrade a real
    PASS into a warning."""
    assert barrier([(PRESENT, 99)]) == 1


def test_the_uncertified_warning_names_the_consequence(barrier, capsys):
    """The warning is the only thing standing between a starved cohort and a
    silent pass, so it must say which seat, that absence was unproven, and what
    the peers will do — convention 6: the output diagnoses itself."""
    barrier([(WITHOUT_OWN_ACK, 12)])  # seat=macos — the ack WITHOUT_OWN_ACK omits
    out = capsys.readouterr().out
    assert "macos" in out
    assert "lossy" in out.lower()
    # It must not read as a pass: the next session has to know the barrier
    # abstained rather than certified this seat.
    assert "PASS" not in out


def test_completeness_of_the_LAST_read_decides(barrier):
    """A read that starts lossy and is COMPLETE at the deadline yields a sound
    absence — so this reds. Otherwise one early unregistered row would excuse
    every genuine starvation for the rest of the window."""
    with pytest.raises(AssertionError) as e:
        barrier([(WITHOUT_OWN_ACK, 12), (WITHOUT_OWN_ACK, 12), WITHOUT_OWN_ACK])
    assert "20260724-06-done-macos.txt" in str(e.value)


def test_a_read_that_never_completes_still_reds(barrier):
    """A lossy read and a FAILED read are different states. A read that never
    completed is not evidence of anything — but it is also not the lazy-list
    gap, and silently passing on it would hide a client that cannot read Media
    at all. Unchanged behaviour: red, with the cause."""
    with pytest.raises(AssertionError) as e:
        barrier([RuntimeError("list never loaded")])
    assert "list never loaded" in str(e.value)


# --- freshness: every poll must re-pull from the nest (run -07's false red) --
#
# Completeness has TWO axes and the gates above pin only one. A read can be
# lossy (rows registered but unreadable — the lazy-list gap; the `rows` yardstick
# catches it) or STALE (the snapshot itself is old — `rows` comes from the same
# old snapshot, so the read looks "complete" while showing the past). Run
# `20260724-07` red-ed on the second axis: the Media listing refreshes only on a
# navigation EDGE (`apps/fauna-tui/src/app.rs::App::apply` — `edge = self.page
# != page`; every app shares the enter-is-the-trigger shape), and the barrier
# re-navigated to a page it was already on, so polls 2..N re-read one frozen
# snapshot for 600 s. The last-finishing seat is structurally the victim: its
# own ack is written seconds before the only effective read, so the upload
# always loses that race. The gates below drive the REAL `_read_set_listing`
# (not the stub seam above) against a driver that models exactly that edge
# semantics — and nothing else.


class _NavEdgeDriver:
    """The one product fact that red-ed `-07`, and nothing else: the Media
    listing is pulled from the nest only when navigation ENTERS the page.
    Every row of the pulled snapshot registers (the terminal client's truth —
    `media/mod.rs::elements` iterates all items), so reads here are never
    lossy; the only way to be wrong is to be stale."""

    def __init__(self, nest: list[str]):
        self.nest = nest  # the nest's live listing — tests mutate it
        self.page: str | None = None
        self.snapshot: list[str] = []  # what the Media page last pulled
        self.detail_open = False

    def navigate_to(self, view: str) -> None:
        if view == "media" and self.page != "media":
            self.snapshot = list(self.nest)  # the nav-edge refresh
        self.page = view

    def select(self, _id: str, _value: str) -> None:
        pass  # `set_filter` is pure render state — never a re-pull

    def count(self, _id: str) -> int:
        return 0 if self.detail_open else len(self.snapshot)

    def get_text(self, _id: str, scope: str = "") -> str:
        index = int(scope.rsplit("[", 1)[1].rstrip("]"))
        return self.snapshot[index]

    def is_visible(self, _id: str, **_kw) -> bool:
        return self.detail_open  # only queried for `media-item-detail`

    def click(self, _id: str, **_kw) -> None:
        self.detail_open = False  # only clicked for the detail close button


class _App:
    def __init__(self, driver: _NavEdgeDriver):
        self.media = MediaActions(driver)


@pytest.fixture
def fresh_reads(monkeypatch):
    """No wall-clock cost for the real `_read_set_listing`/barrier loops."""
    monkeypatch.setattr(live.time, "sleep", lambda _s: None)
    monkeypatch.setattr(cfg._time, "sleep", lambda _s: None)


def test_listing_read_pulls_fresh_state_even_when_already_on_media(fresh_reads):
    """Two consecutive listing reads with a nest-side change between them must
    observe the change — even though the app never left the Media page. This is
    the minimal form of the `-07` false red: under edge-only refresh semantics,
    a read that does not force its own edge returns the previous read's world."""
    driver = _NavEdgeDriver(nest=["20260724-07-hello-linux.txt"])
    app = _App(driver)
    names, _rows = live._read_set_listing(app)
    assert "20260724-07-hello-linux.txt" in names
    driver.nest.append("20260724-07-done-macos.txt")  # the agent's upload drains
    names, rows = live._read_set_listing(app)
    assert "20260724-07-done-macos.txt" in names, (
        "the second read returned the FIRST read's snapshot — the listing read "
        "did not force a nav edge, so it can never observe an upload that lands "
        "while the app sits on the Media page (run 20260724-07's false red)"
    )
    assert rows == len(names)


def test_the_run_20260724_07_frozen_listing_must_not_red_the_barrier(
    fresh_reads, monkeypatch
):
    """The `-07` replay, end to end through the REAL listing read: the app is
    already on Media (phase 0 read it), the seat's own ack reaches the nest
    only after that — exactly the last-finishing seat's structural position —
    and the barrier polls. A barrier whose polls re-pull sees the ack and
    passes; one that re-reads a frozen snapshot burns the whole window and
    fails a healthy run, sending the next session hunting a sync bug that does
    not exist (the most expensive failure mode this test has)."""
    monkeypatch.setattr(live, "RUN_ID", "20260724-07")
    monkeypatch.setattr(live, "WINDOW", 1.0)
    driver = _NavEdgeDriver(
        nest=[
            "20260724-07-done-linux.txt",
            "20260724-07-done-windows.txt",
            "20260724-07-hello-macos.txt",
            "20260724-07-ready-macos.txt",
            "20260724-07-shared.txt",
        ]
    )
    app = _App(driver)
    live._read_set_listing(app)  # phase 0's read leaves the app on Media
    driver.nest.append("20260724-07-done-macos.txt")  # upload drains post-read
    live._await_own_ack_on_nest(app, "macos")  # must NOT raise


def test_listing_read_closes_an_open_item_detail_first(fresh_reads):
    """An open `media-item-detail` owns the pane, so a re-list with it open
    renders ZERO items — an empty library with no error banner, the artifact
    that cost the member-decrypt suite five runs. Any helper whose job is
    "re-enter and re-read" must close it first."""
    driver = _NavEdgeDriver(nest=["20260724-07-shared.txt"])
    app = _App(driver)
    driver.detail_open = True
    names, _rows = live._read_set_listing(app)
    assert names == ["20260724-07-shared.txt"]
