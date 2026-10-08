"""tier_1 gates for the N-seat, one-machine live sync convention — the pure
halves of ``helpers/sync_seats.py`` and ``helpers/convergence_legs.py``, decided
on one machine in milliseconds.

The tests they serve need a nest of some kind — standalone binary, docker image
or live box (testing.md § Default app and nest mode, *Mode mechanics*: by
default a test runs in all three modes) — so everything about them that needs NO
nest at all is decided here: how the run token is minted, that the token can
never perturb the tri-machine announce's id sequence, how a seat set names
itself in the parametrization, what the residue warning says, that the run-file
protocol is genuinely shared with the tri-machine round instead of re-declared
beside it, and — for the three-seat shape — that the legs genuinely FAN OUT
rather than checking neighbours pairwise.

Since the live fold (2026-08-02) and the seat-count fold (2026-08-03) the whole
family is ONE module, ``tests/test_filesync_seats.py`` — every mode-uniform seat
set at both ratified counts (``DEFAULT_SEAT_SETS``), across all three nest
modes. Nothing here should be read as "this shape is live-only".

testing.md § convention 16 owns the normative claims.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from helpers import multiseat_config as cfg
from helpers import sync_seats

import tests.test_filesync_multiseat_live as ms
# Module-import smoke: the seat module must stay importable. Cheap, and it is
# what catches a helper rename that only a live run would otherwise surface —
# and a live run is the one thing this suite cannot do. Since the folds this IS
# the live module too: one file, both seat counts, all three nest modes.
import tests.test_filesync_seats as seats_module  # noqa: F401
from helpers import convergence_legs
from i18n.strings import S

pytestmark = [pytest.mark.tier_1]


class _FakeSeat:
    """A seat with no daemon, no nest and no process — just a folder and a set
    of deletions it admits to having applied."""

    def __init__(self, name, path, applied=()):
        self.name = name
        self.path = path
        self.path.mkdir(parents=True, exist_ok=True)
        self.applied = set(applied)

    def observed_delete(self, basename):
        return basename in self.applied

    def self_note(self):
        return f"  self-check: seat {self.name} (fake)"

    def diagnostics(self):
        return f"  seat {self.name}: fake"


# ── 1. the run token ──────────────────────────────────────────────────────────


def test_the_token_carries_the_date_and_eight_random_hex():
    token = sync_seats.new_run_token(today="20260801", rand_hex="deadbeef")
    assert token == "2seat-20260801-deadbeef"


def test_the_token_leads_with_the_seat_count():
    """Residue on the shared live set must say which run SHAPE left it.

    A three-seat run leaves three seats' worth of files behind if it dies; a
    human reading `3seat-…` in the set knows immediately which test to re-run to
    clean it, without cross-referencing a log that may be long gone."""
    assert (
        sync_seats.new_run_token(seats=3, today="20260802", rand_hex="deadbeef")
        == "3seat-20260802-deadbeef"
    )
    assert sync_seats.token_prefix(2) == "2seat-"
    assert sync_seats.token_prefix(3) == "3seat-"


def test_a_minted_token_is_well_formed_without_any_arguments():
    """The unattended run mints its own — no operator, no announce, no env var."""
    for seats in (2, 3):
        token = sync_seats.new_run_token(seats=seats)
        assert re.fullmatch(rf"{seats}seat-\d{{8}}-[0-9a-f]{{8}}", token), token


def test_two_tokens_minted_back_to_back_differ():
    """Freshness is BY CONSTRUCTION — this is the whole reason the two-seat test
    needs no announce, no Media-UI read and no phase-0 freshness assertion."""
    tokens = {sync_seats.new_run_token() for _ in range(50)}
    assert len(tokens) == 50


def test_the_token_is_a_legal_run_id_for_the_shared_run_file_protocol():
    """It prefixes every run file's basename, so it must survive the same charset
    gate the tri-machine seat applies to its own id."""
    assert re.fullmatch(r"[A-Za-z0-9_-]{1,40}", sync_seats.new_run_token())


def test_residue_can_never_perturb_the_tri_machine_id_sequence():
    """The load-bearing non-collision pin, stated BEHAVIOURALLY.

    A crashed two-seat run leaves ``2seat-*`` files in the shared set. The
    tri-machine announce derives its next id by counting the set's own basenames
    (``next_run_id_from_names``), so a token that the announce could read as a
    ``YYYYMMDD-NN`` run file would silently push the tri-machine sequence
    forward — and the cohort would rendezvous on an id nobody agreed. Asserting
    the regex would only pin today's implementation; asserting the announce's
    ANSWER pins the property.
    """
    today = "20260801"
    residue = [
        sync_seats.new_run_token(today=today, rand_hex="deadbeef") + "-hello-a.txt",
        sync_seats.new_run_token(today=today, rand_hex="0badcafe") + "-shared.txt",
        # ...and the three-seat namespace is disjoint on exactly the same terms.
        # Asserted per seat count rather than once, because the count is now part
        # of the prefix: a future `10seat-` must not become the first token that
        # parses as something the announce counts.
        sync_seats.new_run_token(seats=3, today=today, rand_hex="c0ffee00")
        + "-hello-c.txt",
    ]
    assert cfg.next_run_id_from_names(residue, today) == f"{today}-01"
    # ...and it does not disturb a real sequence either.
    assert (
        cfg.next_run_id_from_names([f"{today}-07-hello-linux.txt", *residue], today)
        == f"{today}-08"
    )


# ── 2. the seat-pair axis ─────────────────────────────────────────────────────


def test_a_pair_names_itself_by_its_two_seat_drivers():
    assert sync_seats.seat_set_id(("tui", "tui")) == "tui+tui"
    assert sync_seats.seat_set_id(("native", "tui")) == "native+tui"


def test_a_seat_set_id_carries_its_seat_count_by_construction():
    """A three-seat run must be distinguishable from a two-seat one in a summary
    line and in flake history, and the id is the only thing carried there."""
    assert (
        sync_seats.seat_set_id(("tui", "tui", "tui"))
        == "tui+tui+tui"
    )


def test_the_trio_matrix_mirrors_the_pair_matrix_one_seat_wider():
    """Same modes at both seat counts, deliberately.

    A hand-picked three-seat subset would let a platform's fan-out coverage
    differ from its pairwise coverage without anybody declaring it — and the
    difference would be invisible, because both runs report green. The platform
    filter is the ONLY thing allowed to remove a set."""
    assert {modes[0] for modes in sync_seats.DEFAULT_TRIOS} == {
        modes[0] for modes in sync_seats.DEFAULT_PAIRS
    }
    for modes in sync_seats.DEFAULT_TRIOS:
        assert len(modes) == 3
        assert len(set(modes)) == 1, "the default trios are homogeneous"
        assert all(mode in sync_seats.SEAT_MODES for mode in modes)


def test_the_platform_filter_is_seat_count_agnostic():
    """One filter for every seat count — the exclusions cannot fall out of step.

    tui cannot drive a sync agent on windows whether there are two seats or
    three, so the trio is filtered out of collection there for the same
    structural reason and by the same code path (convention 7: filtered, never
    skipped)."""
    trios = sync_seats.seat_sets_for_platform("win32", sync_seats.DEFAULT_TRIOS)
    assert ("native", "native", "native") in trios
    assert ("tui", "tui", "tui") not in trios
    assert ("tui", "tui", "tui") in sync_seats.seat_sets_for_platform(
        "linux", sync_seats.DEFAULT_TRIOS
    )


def test_the_seat_pool_is_a_prefix_so_seat_a_means_the_same_thing_everywhere():
    """Seat `a` is the creator and the deleter at every seat count. If the pool
    were re-derived per count, a three-seat failure message naming `a` would not
    be talking about the same role as a two-seat one."""
    assert sync_seats.seat_names(2) == ("a", "b")
    assert sync_seats.seat_names(3) == ("a", "b", "c")
    assert sync_seats.seat_names(3)[: len(sync_seats.seat_names(2))] == (
        sync_seats.seat_names(2)
    )
    # A count the pool cannot name raises rather than silently truncating — two
    # seats sharing one name would make every observation ambiguous.
    for bad in (1, 0, len(sync_seats.SEAT_NAMES) + 1):
        with pytest.raises(ValueError):
            sync_seats.seat_names(bad)


def test_the_default_matrix_is_collectable_and_every_entry_is_a_pair():
    """The matrix collects the tui UI pair and the native-app pair — the
    disk-only pair is retired (2026-09-30: the nest refuses the headless daemon's
    unsigned records, so it can never converge)."""
    pairs = sync_seats.DEFAULT_PAIRS
    assert pairs, "the default matrix must never collect nothing"
    for pair in pairs:
        assert len(pair) == 2
        assert all(mode in sync_seats.SEAT_MODES for mode in pair)
    assert ("engine", "engine") not in pairs
    assert "engine" not in sync_seats.SEAT_MODES
    assert ("tui", "tui") in pairs
    assert ("native", "native") in pairs


def test_every_seat_platform_collects_at_least_the_native_pair():
    """No platform may be left with nothing to run. Since the disk-only seat
    retired, the native pair is what windows collects (its tui pair is a
    structural absence)."""
    for platform in ("linux", "darwin", "win32"):
        assert ("native", "native") in sync_seats.seat_sets_for_platform(platform)


def test_the_tui_pair_is_filtered_out_on_windows_not_skipped():
    """tui drives no sync agent on windows (structural, convention 7) — the pair
    must never be COLLECTED there, because a skip reports `s` in a summary line
    that reads like success."""
    assert ("tui", "tui") not in sync_seats.seat_sets_for_platform("win32")
    assert ("tui", "tui") in sync_seats.seat_sets_for_platform("linux")
    assert ("tui", "tui") in sync_seats.seat_sets_for_platform("darwin")


def test_the_default_matrix_is_both_seat_counts_of_the_same_modes():
    """Ruling 1 of the seat-module fold (2026-08-03): the bare run's default is
    every mode-uniform set at BOTH ratified seat counts — nothing more.

    Defined as the concatenation rather than re-enumerated, so a mode added to
    the pair matrix cannot arrive without its trio (the coverage-drift
    `test_the_trio_matrix_mirrors_the_pair_matrix_one_seat_wider` guards), and
    so the folded module's collected set is exactly the two retired modules'
    union — the fold changed the file count, never the coverage."""
    assert (
        sync_seats.DEFAULT_SEAT_SETS
        == sync_seats.DEFAULT_PAIRS + sync_seats.DEFAULT_TRIOS
    )


def test_the_param_id_leads_with_the_seat_count():
    """Ruling 4 of the seat-module fold (2026-08-03): the parametrization id is
    ``<N>seat-<modes>`` — the SAME prefix vocabulary the run token and live-set
    residue use, so flake history, `-k 2seat`/`-k 3seat` selection (which
    replaced per-file selection when the modules folded) and residue on the
    shared set all correlate without a lookup table.

    Without the count prefix, `-k tui+tui` substring-matches the trio's
    ``tui+tui+tui`` too — there would be no way to select the pairs."""
    assert sync_seats.seat_set_param_id(("tui", "tui")) == "2seat-tui+tui"
    assert (
        sync_seats.seat_set_param_id(("tui", "tui", "tui")) == "3seat-tui+tui+tui"
    )
    for n in (2, 3):
        assert sync_seats.seat_set_param_id(("native",) * n).startswith(
            sync_seats.token_prefix(n)
        )


def test_seat_sets_from_env_makes_the_mixed_bisect_invocable():
    """`$FAUNA_SEAT_SETS` is how a session runs the mixed bisect.

    A multi-seat red says only "these seats disagree"; `native+tui`
    localizes it, because the tui half is the seat the `tui+tui` set already
    proved in the same run. The override existed
    as a `seat_sets_for_platform` argument from day one but had no caller, so the
    only way to reach it was to edit the parametrization — an edit a session
    then has to remember not to commit.

    The set's LENGTH is the seat count, so this is also the three-seat bisect's
    entry point.
    """
    assert sync_seats.seat_sets_from_env({}) is None
    assert sync_seats.seat_sets_from_env({sync_seats.SEAT_SETS_ENV: "   "}) is None
    assert sync_seats.seat_sets_from_env({sync_seats.SEAT_SETS_ENV: "native+tui"}) == (
        ("native", "tui"),
    )
    assert sync_seats.seat_sets_from_env(
        {sync_seats.SEAT_SETS_ENV: "native+tui, tui+native"}
    ) == (("native", "tui"), ("tui", "native"))

    # A three-mode set is a THREE-SEAT run, not an error — this is how a
    # three-seat mixed bisect (`the app seat uploads but does it receive from
    # BOTH peers?`) is invoked without editing the parametrization.
    assert sync_seats.seat_sets_from_env(
        {sync_seats.SEAT_SETS_ENV: "native+tui+tui"}
    ) == (("native", "tui", "tui"),)

    # An unknown mode raises rather than silently collecting nothing — a typo
    # that yields an empty matrix reads exactly like "there was nothing to run".
    # So does a set with fewer than two seats, or more than the pool can name.
    for bad in (
        "windows+tui",
        # The retired disk-only mode is an unknown mode now, never a seat.
        "native+engine",
        "native",
        "native+bogus",
        "+".join(["tui"] * (len(sync_seats.SEAT_NAMES) + 1)),
    ):
        with pytest.raises(ValueError):
            sync_seats.seat_sets_from_env({sync_seats.SEAT_SETS_ENV: bad})

    # The platform filter still applies on top of an explicit request.
    assert sync_seats.seat_sets_for_platform(
        "win32", sync_seats.seat_sets_from_env({sync_seats.SEAT_SETS_ENV: "native+native"})
    ) == (("native", "native"),)


def test_a_platform_filter_removes_a_pair_if_either_seat_is_unsupported():
    """A mixed pair is only as collectable as its weaker seat — filtering on
    `any` rather than `all` is what keeps `tui+native` off windows too."""
    mixed = (("tui", "native"), ("native", "tui"), ("native", "native"))
    assert sync_seats.seat_sets_for_platform("win32", mixed) == (("native", "native"),)
    assert sync_seats.seat_sets_for_platform("linux", mixed) == mixed


def test_the_native_pair_collects_on_every_gui_desktop_platform():
    """`native` is the ONE mode name for "this platform's own GUI app" — the
    vocabulary `SEAT_MODES` and testing.md § convention 16 already use. A
    per-app name (`windows`, `macos`, `linux`) would make each box invent a
    second name for the identical concept (priorities #1/#3), and `seat_set_id`
    then reads `native+native` on all three.

    darwin joined win32 here when the macos seat landed and linux followed 2026-08-10, closing the arm: each time the seat was
    one row in `NATIVE_APPS` because `make_seat` already resolved the mode
    through it, so what actually gated collection was the `UNBUILT_MODES` entry
    below."""
    for platform in ("win32", "darwin", "linux"):
        assert ("native", "native") in sync_seats.seat_sets_for_platform(platform)


def test_no_platform_owes_an_unbuilt_seat_any_more():
    """`UNBUILT_MODES` reached `{}` on 2026-08-10 with the linux seat — the state
    the map was always shaped to arrive at, and the reason it was kept apart
    from `UNSUPPORTED_MODES` in the first place.

    The distinction convention 7 exists to keep: `tui` on windows CANNOT exist
    (the terminal client drives no agent there) and is closed forever, while a
    missing native seat was only ever debt with a named owner. Filing debt as
    *unsupported* would have taught every future reader it was impossible —
    which is how an owed track dies.

    The empty map is asserted directly rather than per-platform because a stale
    entry for ANY platform silently filters that platform's pair right back out
    of collection, and the next reader would have no reason to look."""
    assert sync_seats.UNBUILT_MODES == {}
    for platform in ("win32", "darwin", "linux"):
        assert "native" not in sync_seats.UNSUPPORTED_MODES.get(platform, ())
        assert ("native", "native") in sync_seats.seat_sets_for_platform(platform)
    # And the structural map keeps its own meaning intact — emptying the debt
    # map must not be read as "every exclusion is gone".
    assert "tui" in sync_seats.UNSUPPORTED_MODES["win32"]
    assert ("tui", "tui") not in sync_seats.seat_sets_for_platform("win32")


def test_every_unbuilt_entry_names_the_owner_that_owes_it():
    """Debt with no named owner is indistinguishable from debt nobody intends to
    pay. Each entry's reason has to be specific enough to act on cold.

    Vacuous while the map is `{}` — which is the point of the seeded half below:
    the shape check has to still be running when the NEXT platform files debt,
    or it comes back as a rule nobody enforced in the interim."""
    for platform, modes in sync_seats.UNBUILT_MODES.items():
        for mode, reason in modes.items():
            assert mode in sync_seats.SEAT_MODES, (platform, mode)
            assert len(reason) > 20, f"{platform}/{mode} reason is too thin: {reason!r}"


def test_the_unbuilt_note_names_what_a_platform_is_not_collecting(monkeypatch):
    """No silent caps: a platform that collects fewer sets than the matrix
    declares must SAY so, or a green run reads as full coverage.

    Driven off a SEEDED map since 2026-08-10, when the linux seat emptied the
    real one. The alternative — deleting this gate as unreachable — is what
    would make the machinery rot unnoticed: the next platform to file debt would
    inherit a printer no test has exercised since the day it went quiet, and the
    failure mode is silence, which is exactly what nobody notices."""
    monkeypatch.setattr(
        sync_seats,
        "UNBUILT_MODES",
        {"plan9": {"native": "no plan9 app seat yet — owner: NEXT-plan9.md row 1"}},
    )
    note = sync_seats.unbuilt_note("plan9")
    assert note is not None
    assert "native" in note and "NEXT-plan9.md" in note
    assert sync_seats.unbuilt_note("linux") is None


def test_no_real_platform_prints_an_unbuilt_note_any_more():
    """The note going quiet is the visible half of a seat landing — all three
    GUI desktops now build their native arm, so a note reappearing on any of
    them means a debt entry came back rather than a new platform arriving."""
    for platform in ("linux", "win32", "darwin"):
        assert sync_seats.unbuilt_note(platform) is None


def test_the_seats_are_named_for_their_role_not_for_a_machine():
    """``linux``/``macos``/``windows`` identify MACHINES in the tri-machine round;
    seats on one machine are not machines, so they are ``a``, ``b``, ``c``."""
    assert sync_seats.SEAT_NAMES == ("a", "b", "c")
    assert not set(sync_seats.SEAT_NAMES) & set(ms._ALL_SEATS)


# ── 2b. the leg plans FAN OUT (the property two seats cannot state) ───────────


def test_the_hello_leg_reaches_every_peer_not_just_the_next_one():
    """The single most likely way to break a three-seat run.

    Reusing a pairwise leg body — or walking the seats as a chain — yields
    a → b and b → c, both green, while ``a → c`` is never checked at all: one
    seat receives from nobody and the run still passes. Enumerating the peers is
    what makes that unrepresentable, so the enumeration itself is pinned.
    """
    plan = convergence_legs.hello_fanout_plan(("a", "b", "c"))
    assert plan == [
        ("a", ["b", "c"]),
        ("b", ["a", "c"]),
        ("c", ["a", "b"]),
    ]
    # The property, stated independently of the literal above: every ordered
    # pair of distinct seats appears exactly once.
    observed = {(w, o) for w, obs in plan for o in obs}
    assert observed == {
        (w, o) for w in "abc" for o in "abc" if w != o
    }
    assert len(observed) == 6, "3 seats owe 6 observations, not 3"


def test_the_fan_out_plan_reduces_to_the_historical_two_seat_legs():
    """Two seats must keep running exactly what they always ran — the
    generalization is not allowed to change the shape of the shipped test."""
    assert convergence_legs.hello_fanout_plan(("a", "b")) == [
        ("a", ["b"]),
        ("b", ["a"]),
    ]


def test_peers_never_includes_the_seat_itself():
    """A seat awaiting its OWN write would be a tautology that passes with the
    nest switched off entirely — the leg would assert nothing about sync."""
    assert convergence_legs.peers(("a", "b", "c"), "b") == ["a", "c"]
    assert convergence_legs.peers(("a", "b"), "a") == ["b"]


def test_the_observation_count_grows_with_the_seat_count():
    """The timeout ceilings are DERIVED from this, so a leg added to the plan
    without updating the count would silently under-bound every module."""
    # 2 seats: 2 fan-out + 1 merge base + 2 merge + 1 anchor base + 2 anchor
    #          fold + 1 delete = 9
    assert convergence_legs.await_count(2) == 9
    # 3 seats: 6 fan-out + 2 merge base + 3 merge + 2 anchor base + 3 anchor
    #          fold + 2 delete = 18
    assert convergence_legs.await_count(3) == 18
    assert convergence_legs.await_count(3) > convergence_legs.await_count(2)


# ── 2b. leg 4a: the same-anchor append fold ───────────────────────────────────


def test_every_seat_appends_a_DISTINCT_line_at_the_SAME_anchor(tmp_path):
    """The shape leg 4 cannot express, pinned at its two ends.

    Leg 4's edits replace each seat's OWN pre-existing line, which
    ``ms._shared`` separates with unique spacers precisely so every pairwise
    hunk merges cleanly — the merged bytes are therefore known in advance. Leg
    4a is the opposite by construction: one base, every seat's hunk sharing the
    end-of-file anchor, so the fold is ORDER-DEPENDENT and no permutation can be
    predicted (`conflicts.md` § Concurrent resolution, clause 5's gap block;
    tier_1 model
    ``merge_convergence_test.rs::concurrent_same_anchor_appends_converge_at_three_and_four_seats``).

    So the two things this leg's protocol must guarantee are pinned here: every
    seat's marker is distinct (else a "lost append" is unobservable), and every
    file it writes carries the run token (else the live box's residue reaping
    and the cleanup leg's glob both miss it).
    """
    token = "3seat-20260804-deadbeef"
    markers = [convergence_legs.anchor_marker(n, token) for n in ("a", "b", "c")]
    assert len(set(markers)) == 3, f"markers must be distinct: {markers}"
    assert all(token in m for m in markers)
    assert token in convergence_legs.anchor_base(token)

    path = convergence_legs.anchor_path(tmp_path, token)
    assert path.name.startswith(f"{token}-"), (
        f"{path.name} escapes the run token's namespace: the cleanup leg globs "
        f"'{token}-*' and the live box reaps on the same prefix, so a file "
        f"outside it leaks onto the shared set permanently"
    )

    # Every seat appends at the SAME anchor — the base is a strict prefix of
    # every seat's edit, and the seats differ only in the appended line.
    base = convergence_legs.anchor_base(token)
    edits = [convergence_legs.anchor_edit(n, token) for n in ("a", "b", "c")]
    for name, edit in zip(("a", "b", "c"), edits):
        assert edit.startswith(base), (
            "an append must keep the shared base verbatim — an edit that "
            "rewrites it is a different-anchor edit wearing this leg's name"
        )
        assert edit == base + convergence_legs.anchor_marker(name, token) + "\n"


def test_the_fold_barrier_returns_the_instant_every_seat_agrees(tmp_path):
    """Green runs pay nothing (convention 14): the budget bounds the failure,
    it is not a settle-window the leg waits out."""
    token = "3seat-20260804-deadbeef"
    seats = [_FakeSeat(n, tmp_path / n) for n in ("a", "b", "c")]
    # The converged state: ONE permutation, on every seat, each marker once.
    converged = convergence_legs.anchor_base(token) + "".join(
        convergence_legs.anchor_marker(n, token) + "\n" for n in ("b", "a", "c")
    )
    for seat in seats:
        convergence_legs.anchor_path(seat.path, token).write_text(converged)

    got = convergence_legs.await_same_anchor_fold(seats, token, 5.0, lambda: "")
    assert got == converged


def test_the_fold_barrier_refuses_a_PERMUTATION_disagreement(tmp_path):
    """FINDING B's exact defect shape: no edit is lost and nothing is
    duplicated — the seats simply hold different permutations forever, because
    each skipped its peers' resolutions as stale
    (`merge_convergence_test.rs`'s finding-B doc comment). A leg that asserted
    only "every marker is present" would pass it, which is why agreement is a
    separate conjunct."""
    token = "3seat-20260804-deadbeef"
    seats = [_FakeSeat(n, tmp_path / n) for n in ("a", "b", "c")]
    orders = (("a", "b", "c"), ("b", "a", "c"), ("c", "a", "b"))
    for seat, order in zip(seats, orders):
        convergence_legs.anchor_path(seat.path, token).write_text(
            convergence_legs.anchor_base(token)
            + "".join(convergence_legs.anchor_marker(n, token) + "\n" for n in order)
        )

    with pytest.raises(AssertionError) as excinfo:
        convergence_legs.await_same_anchor_fold(seats, token, 0.5, lambda: "")
    assert "DISAGREE" in str(excinfo.value)


def test_the_fold_barrier_refuses_a_DUPLICATED_append(tmp_path):
    """The runaway signature (`check_finals`: re-merging already-incorporated
    content from a stale ancestor duplicates lines, and the duplicates feed the
    next round). Every seat agrees here, so agreement alone would pass it."""
    token = "3seat-20260804-deadbeef"
    seats = [_FakeSeat(n, tmp_path / n) for n in ("a", "b", "c")]
    doubled = convergence_legs.anchor_base(token) + "".join(
        convergence_legs.anchor_marker(n, token) + "\n" for n in ("a", "b", "b", "c")
    )
    for seat in seats:
        convergence_legs.anchor_path(seat.path, token).write_text(doubled)

    with pytest.raises(AssertionError) as excinfo:
        convergence_legs.await_same_anchor_fold(seats, token, 0.5, lambda: "")
    assert "DUPLICATED" in str(excinfo.value)


def test_the_fold_barrier_refuses_a_LOST_append(tmp_path):
    """Agreement bought by dropping a side. The tier_1 model tolerates this for
    the append shape under adversarial re-delivery schedules
    (`check_finals`'s "no lost edit" conjunct is OwnLine-only), but the e2e leg
    induces no such churn: every seat writes once and quiesces, which is the
    empty schedule the model asserts exactly-once for. So a lost append here is
    a finding, not a sanctioned degrade — and the message has to say which seat
    lost whose line."""
    token = "3seat-20260804-deadbeef"
    seats = [_FakeSeat(n, tmp_path / n) for n in ("a", "b", "c")]
    lost = convergence_legs.anchor_base(token) + "".join(
        convergence_legs.anchor_marker(n, token) + "\n" for n in ("a", "c")
    )
    for seat in seats:
        convergence_legs.anchor_path(seat.path, token).write_text(lost)

    with pytest.raises(AssertionError) as excinfo:
        convergence_legs.await_same_anchor_fold(seats, token, 0.5, lambda: "")
    message = str(excinfo.value)
    assert "LOST" in message and "b" in message


def test_the_fold_barrier_reports_a_seat_the_file_never_reached(tmp_path):
    """Absence is its own diagnosis: "c has no anchor file" and "c disagrees"
    are different findings and must not read alike."""
    token = "3seat-20260804-deadbeef"
    seats = [_FakeSeat(n, tmp_path / n) for n in ("a", "b", "c")]
    body = convergence_legs.anchor_base(token) + "".join(
        convergence_legs.anchor_marker(n, token) + "\n" for n in ("a", "b", "c")
    )
    for seat in seats[:2]:
        convergence_legs.anchor_path(seat.path, token).write_text(body)

    with pytest.raises(AssertionError) as excinfo:
        convergence_legs.await_same_anchor_fold(seats, token, 0.5, lambda: "")
    assert "never reached" in str(excinfo.value)


# ── 3. the residue warning ────────────────────────────────────────────────────


def test_no_residue_reads_as_no_warning(tmp_path):
    """A green run's cleanup leg deletes everything, so the finalizer is silent."""
    folder = tmp_path / "a"
    folder.mkdir()
    (folder / "20260730-02-hello-linux.txt").write_text("someone else's run\n")
    assert sync_seats.residue_note("2seat-20260801-deadbeef", [("a", folder)]) is None


def test_residue_is_named_file_by_file_and_seat_by_seat(tmp_path):
    """The carve-out that licenses an unattended live run REQUIRES the run to
    remove what it created, so leftover files must be named loudly enough that a
    human can finish the job — never a bare "cleanup failed"."""
    token = "2seat-20260801-deadbeef"
    a, b = tmp_path / "a", tmp_path / "b"
    for folder in (a, b):
        folder.mkdir()
    (a / f"{token}-hello-a.txt").write_text("x")
    (a / f"{token}-shared.txt").write_text("x")
    (b / f"{token}-hello-a.txt").write_text("x")
    (b / "20260730-02-hello-linux.txt").write_text("not mine")

    note = sync_seats.residue_note(token, [("a", a), ("b", b)])
    assert note is not None
    assert f"{token}-hello-a.txt" in note
    assert f"{token}-shared.txt" in note
    assert "seat a" in note and "seat b" in note
    # Another run's files are NOT this run's to report or to delete.
    assert "20260730-02-hello-linux.txt" not in note


def test_a_missing_folder_is_not_residue(tmp_path):
    """A seat that never started has no folder; that is not a leak to report."""
    assert sync_seats.residue_note("2seat-20260801-deadbeef", [("a", tmp_path / "gone")]) is None


# ── 4. the run-file protocol is SHARED, not re-declared ───────────────────────


def test_the_shared_body_is_built_by_the_tri_machine_round_s_own_builder(tmp_path):
    """Two seats that disagreed on a single byte would never converge, and the
    spacer-anchored hunk shape is what makes the concurrent three-way merge clean
    in any arrival order. Re-declaring it here would fork that reasoning."""
    token = "2seat-20260801-deadbeef"
    body = ms._shared({"a": "base", "b": "base"}, run_id=token, seats=("a", "b"))
    assert body == (
        f"multiseat shared file, run {token}\n"
        "a: base\n"
        "spacer-0a\n"
        "spacer-0b\n"
        "b: base\n"
    )


def test_each_seat_s_hunk_is_a_single_line_between_unique_anchors():
    """The merge property, asserted rather than assumed: every seat's line is
    surrounded by context lines that appear exactly once in the file."""
    token = "2seat-20260801-deadbeef"
    lines = ms._shared(
        {"a": "base", "b": "base"}, run_id=token, seats=("a", "b")
    ).splitlines()
    for line in lines:
        assert lines.count(line) == 1, f"{line!r} is not a unique anchor"


def test_the_hello_body_and_path_carry_the_token():
    token = "2seat-20260801-deadbeef"
    folder = Path("/tmp/seat-a")
    assert ms._hello("a", run_id=token) == f"hello from a in run {token}\n"
    assert ms._hello_path(folder, "a", run_id=token).name == f"{token}-hello-a.txt"
    assert ms._shared_path(folder, run_id=token).name == f"{token}-shared.txt"


def _app_seat(name: str, folder: Path, *, agent_data_dir: Path | None = None):
    """An :class:`AppSeatDriver` that has never been started — enough to pin the
    pure observation half with no driver, no app and no nest in sight."""
    folder.mkdir(parents=True, exist_ok=True)
    return sync_seats.AppSeatDriver(
        "tui",
        name=name,
        run_token="2seat-20260802-deadbeef",
        path=folder,
        launch_config={},
        sign_in=lambda app, seat: None,
        folder="twoseat-local",
        node_url="http://127.0.0.1:1",
        agent_data_dir=agent_data_dir,
    )


def test_an_app_seat_reports_a_delete_only_after_witnessing_the_file(tmp_path):
    """Absence alone is NOT an applied delete: it is equally satisfied by a file
    that never arrived, which is the failure the cleanup leg exists to catch."""
    seat = _app_seat("b", tmp_path / "b")
    name = "2seat-20260802-deadbeef-hello-a.txt"

    # Never seen here → absence proves nothing, so the answer stays False and the
    # leg fails naming the file.
    assert seat.observed_delete(name) is False

    # Seen present → still held, so not deleted.
    (seat.path / name).write_text("hello from a\n")
    assert seat.observed_delete(name) is False

    # Present → absent, witnessed by this seat: that IS the applied delete.
    (seat.path / name).unlink()
    assert seat.observed_delete(name) is True


def test_an_app_seat_ignores_another_runs_files(tmp_path):
    """The witness is scoped to this run's token — a tri-machine run file sitting
    in the folder must never be mistaken for this run's evidence."""
    seat = _app_seat("b", tmp_path / "b")
    (seat.path / "20260730-02-hello-linux.txt").write_text("another run's\n")
    seat.observed_delete("20260730-02-hello-linux.txt")
    assert seat._ever_held == set(), "only <token>-* files are this run's to witness"


def test_an_app_seats_self_note_never_accuses_the_peer(tmp_path):
    """testing.md § point 6: a failure names the direction that broke, from
    evidence the failing party actually has. An app seat has no peer evidence."""
    seat = _app_seat("b", tmp_path / "b")
    (seat.path / "2seat-20260802-deadbeef-hello-b.txt").write_text("mine\n")
    note = seat.self_note()
    assert "seat b" in note
    assert "hello-b" in note, "the note must show what this seat itself wrote"
    for accusation in ("peer", "the other seat", "seat a"):
        assert accusation not in note, (
            f"the self-check names {accusation!r} — it must reason only from "
            f"evidence THIS seat holds"
        )


def test_an_app_seat_with_nowhere_to_look_says_so_rather_than_going_quiet(tmp_path):
    """A seat with no pinned agent dir AND no launched driver has genuinely
    nowhere to look, and must SAY so rather than silently omit the section — an
    absent section reads as "there was nothing to report".

    This is now the only arm that may report "cannot locate": a *launched* unix
    seat resolves its log out of the driver's private world (the test below), so
    "not pinned" stopped being a reason to go quiet on 2026-08-28."""
    seat = _app_seat("b", tmp_path / "b")
    note = seat.agent_log_note()
    assert "agent log" in note
    assert "not locatable" in note, (
        "a seat with nowhere to look must name the reason it cannot look; "
        "silence here is indistinguishable from an empty log"
    )


def test_an_unpinned_seat_finds_its_agent_log_in_the_drivers_private_world(tmp_path):
    """The arm that was missing until 2026-08-28, and whose absence cost a real
    diagnosis: on tui and macOS nothing pins the agent dir, so the note read
    "not pinned" and the one artifact explaining the seat's RECEIVE path had to
    be recovered by hand with `lsof` against the live process — inside the 180 s
    window, or not at all.

    Nothing needed pinning. The agent always wrote inside the launch's own
    private dirs, which the driver already exposes as `config_home`. The layout
    below is the real macOS-hosted tui one observed live (`<launch
    root>/home/Library/Application Support/Fauna/sync/logs/`), deliberately NOT
    the flat pinned shape, so this fails if the search ever narrows back to a
    single hard-coded data-dir rule."""
    launch_root = tmp_path / "fauna-e2e-tui-agent-xyz"
    (launch_root / "config").mkdir(parents=True)
    logs = launch_root / "home" / "Library" / "Application Support" / "Fauna" / "sync" / "logs"
    logs.mkdir(parents=True)
    (logs / "fauna.log.2026-08-28").write_text(
        "sync mode is unresolved; declining a peer's delete\n", encoding="utf-8"
    )

    seat = _app_seat("b", tmp_path / "b")

    class _LaunchedDriver:
        config_home = str(launch_root / "config")

    seat._driver = _LaunchedDriver()

    note = seat.agent_log_note()
    assert "fauna.log.2026-08-28" in note, "the note must name the file it read"
    assert "declining a peer's delete" in note, "and carry the agent's own words"
    assert "not locatable" not in note
    assert "fauna.log.2026-08-28" in seat.diagnostics()


def test_a_macos_seat_finds_its_agent_log_through_the_drivers_state_base(tmp_path):
    """The macOS driver has no `config_home` (apple has none at all) — it
    publishes where its launch's agent keeps state as `sync_agent_state_base`
    instead. Before this arm existed the native-pair scenario run on macOS died at
    seat launch (`_await_adopted_rescan` could not read a log to pin the adopted
    rescan cadence against), with the note `<not locatable on macos>`."""
    state_base = tmp_path / "home" / "Library" / "Application Support" / "Fauna" / "sync"
    logs = state_base / "logs"
    logs.mkdir(parents=True)
    (logs / "fauna.log.2026-10-01").write_text(
        "rescan tick armed every 2311000 ms\n", encoding="utf-8"
    )

    seat = _app_seat("a", tmp_path / "a")

    class _MacosDriver:
        sync_agent_state_base = str(state_base)

    seat._driver = _MacosDriver()

    note = seat.agent_log_note()
    assert "not locatable" not in note
    assert "fauna.log.2026-10-01" in note, "the note must name the file it read"


def test_a_pinned_agent_dir_surfaces_the_agents_own_rolling_log(tmp_path):
    """The gap this closes (testing.md § convention 16): an app seat's own
    `fauna-sync-agent` log — the ONE artifact that explains the receive path —
    was never in the failure message, while the engine seat's was."""
    agent_dir = tmp_path / "sync-agent"
    (agent_dir / "logs").mkdir(parents=True)
    (agent_dir / "logs" / "fauna.log.2026-08-02").write_text(
        "\n".join(f"line {i}" for i in range(60)) + "\n", encoding="utf-8"
    )
    seat = _app_seat("b", tmp_path / "b", agent_data_dir=agent_dir)

    note = seat.agent_log_note()
    assert "fauna.log.2026-08-02" in note, "the note must name the file it read"
    assert "line 59" in note, "the TAIL is what matters — the newest lines"
    assert "line 0" not in note, "a 60-line log must be tailed, not dumped whole"
    # And it must reach the failure message the legs actually print.
    assert "fauna.log.2026-08-02" in seat.diagnostics()


def test_the_apps_pipe_polling_never_crowds_the_sync_lines_out_of_the_tail(tmp_path):
    """The 2026-09-22b 3-seat native delete red printed three 40-line tails of
    nothing but `ListEngines` DEBUG requests — the app polls its agent several
    times a second — so the one line saying what the receive path did with the
    tombstone was scrolled out on every seat. The polling is dropped, and the
    note says how much was dropped so a filtered tail never reads as a quiet
    agent."""
    agent_dir = tmp_path / "sync-agent"
    (agent_dir / "logs").mkdir(parents=True)
    poll = (
        "2026-09-22T07:16:39.324962Z DEBUG fauna_sync_agent::pipe_server: "
        'pipe request id=1 method="ListEngines"'
    )
    (agent_dir / "logs" / "fauna.log.2026-09-22").write_text(
        "\n".join(
            ["2026-09-22T07:16:00Z  INFO fauna_sync_engine: remote delete declined"]
            + [poll] * 500
        )
        + "\n",
        encoding="utf-8",
    )
    seat = _app_seat("b", tmp_path / "b", agent_data_dir=agent_dir)

    note = seat.agent_log_note()
    assert "remote delete declined" in note, "the sync line must survive the polling"
    assert "ListEngines" not in note
    assert "500 pipe-request DEBUG lines omitted" in note


def test_the_newest_rolling_log_wins_when_a_run_crosses_midnight(tmp_path):
    """`fauna_log::init` rolls DAILY, so a long run leaves several files; reading
    the first one alphabetically-by-accident would show yesterday's agent."""
    agent_dir = tmp_path / "sync-agent"
    (agent_dir / "logs").mkdir(parents=True)
    (agent_dir / "logs" / "fauna.log.2026-08-01").write_text("yesterday\n", encoding="utf-8")
    (agent_dir / "logs" / "fauna.log.2026-08-02").write_text("today\n", encoding="utf-8")
    seat = _app_seat("b", tmp_path / "b", agent_data_dir=agent_dir)

    note = seat.agent_log_note()
    assert "today" in note
    assert "yesterday" not in note


def test_a_pinned_agent_dir_that_never_produced_a_log_is_named_as_such(tmp_path):
    """An agent that never started writes no log at all — and that is itself the
    diagnosis (the spawn silently did nothing, the 2026-07-24 failure). It must
    read as "the agent wrote nothing", never as a missing section."""
    agent_dir = tmp_path / "sync-agent"
    agent_dir.mkdir()
    seat = _app_seat("b", tmp_path / "b", agent_data_dir=agent_dir)

    note = seat.agent_log_note()
    assert str(agent_dir) in note, "name the dir that was searched"
    assert "no log" in note


def test_the_windows_agent_data_dir_has_ONE_owner(tmp_path):
    """`isolate_windows_sync_agent` pins the dir and the seat reads the log out of
    it — two call sites, so the path must come from one function, not two
    literals that can drift apart."""
    config = sync_seats.isolate_windows_sync_agent(
        {}, root=tmp_path, seat="a", agent_bin="C:/nowhere/fauna-sync-agent.exe"
    )
    pinned = config["environment"]["FAUNA_E2E_SYNC_AGENT_DATA_DIR"]
    assert Path(pinned) == sync_seats.windows_agent_data_dir(tmp_path)


class _Mode:
    def __init__(self, is_live: bool):
        self.is_live = is_live


@pytest.mark.parametrize(
    "node_url, is_live, custody_first",
    [
        ("http://127.0.0.1:1", False, True),  # standalone
        ("https://box.example:8443", True, True),  # live: real TLS
        ("https://127.0.0.1:1", False, False),  # docker: self-signed, seam cannot dial
    ],
)
def test_the_seat_runs_folder_is_created_custody_first(
    monkeypatch, node_url, is_live, custody_first
):
    """The set nonce every seat signs under must rest in the account's custody
    before a seat launches: an admin-created set has none, and a macos or
    windows seat (mock conversations backend, so no launch-time set-custody
    reconcile) then records unsigned for the whole run — the 2026-10-01
    `signature_required` red on both. Only the self-signed harness nest still
    takes the admin create."""
    calls = []
    monkeypatch.setattr(
        seats_module, "user_create_folder", lambda *a, **k: calls.append(("user", k))
    )
    monkeypatch.setattr(
        seats_module, "create_folder", lambda *a, **k: calls.append(("admin", k))
    )

    seats_module.create_run_set(
        1,
        "2seat",
        "aa" * 32,
        node_url=node_url,
        secret_key="bb" * 32,
        admin_sk=object(),
        nest_mode=_Mode(is_live),
    )

    assert [kind for kind, _ in calls] == ["user" if custody_first else "admin"]
    if custody_first:
        assert calls[0][1]["secret_key"] == "bb" * 32


def test_the_tri_machine_call_sites_are_byte_for_byte_unchanged(monkeypatch):
    """The override is ADDITIVE: omitting it must reproduce exactly what the
    three-machine round has always written, or a mixed run stops converging.

    ``monkeypatch`` restores both sinks ``_publish_run_id`` writes, so this test
    cannot leak an id into a co-collected tri-machine round — the same
    contamination the two-seat test avoids by never calling it at all.
    """
    monkeypatch.setattr(ms, "RUN_ID", "20260801-03")
    monkeypatch.setenv("FAUNA_MULTISEAT_RUN_ID", "20260801-03")

    assert ms._hello("linux") == "hello from linux in run 20260801-03\n"
    assert ms._shared({s: "base" for s in ms.SEATS}) == ms._shared(
        {s: "base" for s in ms.SEATS}, run_id="20260801-03", seats=ms.SEATS
    )
    assert (
        ms._hello_path(Path("/f"), "linux").name == "20260801-03-hello-linux.txt"
    )
    assert ms._shared_path(Path("/f")).name == "20260801-03-shared.txt"


class _FakeBackups:
    """Just enough Folders page for `_folder_index` to read rows."""

    def __init__(self, titles):
        self._titles = list(titles)

    def folder_count(self) -> int:
        return len(self._titles)

    def folder_title(self, i: int) -> str:
        return self._titles[i]


def test_the_set_name_override_is_additive_like_the_run_id_one():
    """The UI steps grew a `set_name` override for the two-seat shape the same
    way the run-file protocol grew `run_id`: omitted, the tri-machine behaviour
    is unchanged, so a mixed run still finds the same row."""
    b = _FakeBackups(["twoseat-local (sync)", f"{ms.SET_NAME} (sync)"])
    # Omitted → the tri-machine set, exactly as before.
    assert ms._folder_index(b) == 1
    # Supplied → the two-seat set, and NOT the tri-machine one.
    assert ms._folder_index(b, "twoseat-local") == 0
    assert ms._folder_index(b, "no-such-set") is None


# ── 5. the cleanup leg's barrier ──────────────────────────────────────────────


def test_a_watcher_whose_window_opens_late_still_proves_the_delete(tmp_path):
    """THE RACE, first hit by ``[3seat-tui+tui+tui]``'s first-ever run
    (2026-08-03): the cleanup leg awaits its watchers SEQUENTIALLY, so the
    second app watcher's polling window opens only after the first watcher's
    full await — by which time it has usually already applied the deletes. Its
    present→absent transition is then unwitnessed and the leg reds after the
    full budget while every folder is correctly empty: a false RED on a healthy
    product (the safe direction, but a defunct verdict all the same).

    The fix is a causal barrier, not a faster poll: leg 5 records the PRESENT
    half on every watcher *before* the deleter unlinks
    (:func:`convergence_legs.prime_delete_witness`), so a window that opens
    arbitrarily late still proves the transition. Primed-then-absent must
    return True without paying any budget."""
    seat = _app_seat("c", tmp_path / "c")
    names = [
        "2seat-20260802-deadbeef-hello-a.txt",
        "2seat-20260802-deadbeef-shared.txt",
    ]
    for name in names:
        (seat.path / name).write_text("x")

    convergence_legs.prime_delete_witness([seat], names)

    # Propagation "beats the first poll": the files are gone before the
    # watcher's await ever runs.
    for name in names:
        (seat.path / name).unlink()

    convergence_legs.await_applied_deletes(seat, names, 5.0, lambda: "")


def test_priming_never_invents_a_witness_for_a_file_that_never_arrived(tmp_path):
    """Fail-toward-RED survives the priming: a file absent at priming time was
    never present here, so it is not recorded, and its delete can never be
    reported as applied — the genuinely-broken-propagation case still reds."""
    seat = _app_seat("c", tmp_path / "c")
    present = "2seat-20260802-deadbeef-hello-a.txt"
    never_arrived = "2seat-20260802-deadbeef-shared.txt"
    (seat.path / present).write_text("x")

    convergence_legs.prime_delete_witness([seat], [present, never_arrived])
    (seat.path / present).unlink()

    assert seat.observed_delete(present) is True
    assert seat.observed_delete(never_arrived) is False


def test_a_confirmed_delete_barrier_returns_without_paying_its_budget(tmp_path):
    """Green runs pay nothing (convention 14) — the budget only makes the
    failure sound."""
    names = ["2seat-20260801-deadbeef-hello-a.txt", "2seat-20260801-deadbeef-shared.txt"]
    peer = _FakeSeat("b", tmp_path / "b", applied=names)
    convergence_legs.await_applied_deletes(peer, names, 5.0, lambda: "")


def test_an_unapplied_delete_names_exactly_what_is_outstanding(tmp_path):
    """The message has to name the files: they are still on the shared live set,
    and the carve-out's whole obligation is that a human can finish the job from
    the log alone."""
    applied = ["2seat-20260801-deadbeef-hello-a.txt"]
    outstanding = "2seat-20260801-deadbeef-shared.txt"
    peer = _FakeSeat("b", tmp_path / "b", applied=applied)
    # A tiny budget with a tiny cadence: the outstanding file never becomes
    # applied, so the outcome is deterministic no matter how long it takes.
    with pytest.raises(AssertionError) as exc:
        convergence_legs.await_applied_deletes(
            peer, [*applied, outstanding], 0.05, lambda: "note", interval=0.01
        )
    message = str(exc.value)
    assert outstanding in message
    assert applied[0] not in message, "already-applied deletes are not outstanding"
    assert "NEW coverage" in message, "a red here must not read as a settled defect"


# ── 6. the finalizer ──────────────────────────────────────────────────────────


def test_a_cleaned_up_run_leaves_the_finalizer_silent(tmp_path, capsys):
    """Leg 5 already deleted everything and proved it — the finalizer says
    nothing at all."""
    seats = [_FakeSeat("a", tmp_path / "a"), _FakeSeat("b", tmp_path / "b")]
    (seats[0].path / "20260730-02-hello-linux.txt").write_text("another run's")
    sync_seats.finalize_live_residue("2seat-20260801-deadbeef", seats, budget=5.0)
    assert capsys.readouterr().out == ""
    assert (seats[0].path / "20260730-02-hello-linux.txt").exists()


def test_the_finalizer_deletes_on_one_seat_and_confirms_on_the_peer(tmp_path, capsys):
    """Deleting in BOTH folders would destroy the only evidence the files left
    the NEST: a seat that deleted its own copy never applies the peer's
    tombstone. So exactly one seat deletes, and the other is the witness."""
    token = "2seat-20260801-deadbeef"
    names = [f"{token}-hello-a.txt", f"{token}-shared.txt"]
    a = _FakeSeat("a", tmp_path / "a")
    b = _FakeSeat("b", tmp_path / "b", applied=names)
    for seat in (a, b):
        for name in names:
            (seat.path / name).write_text("x")

    sync_seats.finalize_live_residue(token, [a, b], budget=5.0)

    out = capsys.readouterr().out
    assert "deleted" in out and "seat a" in out
    assert "⚠️" not in out, "a confirmed cleanup must not warn"
    # Local half of the carve-out: nothing this run created is left anywhere.
    assert not list(a.path.glob(f"{token}-*"))
    assert not list(b.path.glob(f"{token}-*"))


def test_an_unconfirmed_delete_warns_that_the_files_are_probably_still_live(tmp_path, capsys):
    token = "2seat-20260801-deadbeef"
    a = _FakeSeat("a", tmp_path / "a")
    b = _FakeSeat("b", tmp_path / "b")  # applies nothing
    (a.path / f"{token}-shared.txt").write_text("x")

    sync_seats.finalize_live_residue(token, [a, b], budget=0.0)

    out = capsys.readouterr().out
    assert "⚠️" in out
    assert f"{token}-shared.txt" in out
    assert "still on" in out


def test_the_finalizer_never_masks_the_body_s_own_failure(tmp_path, capsys):
    """It runs inside a ``finally`` that may already be unwinding a real
    failure, so it must swallow its own."""
    class _Exploding(_FakeSeat):
        def observed_delete(self, basename):
            raise RuntimeError("boom")

    token = "2seat-20260801-deadbeef"
    a = _Exploding("a", tmp_path / "a")
    b = _Exploding("b", tmp_path / "b")
    (a.path / f"{token}-shared.txt").write_text("x")

    sync_seats.finalize_live_residue(token, [a, b], budget=5.0)  # must not raise

    assert "finalizer itself failed" in capsys.readouterr().out


# ── 8. the conflict review list (leg 4b) ──────────────────────────────────────

_MERGED = S.devices.conflicts.resolved_merged
_LATEST = S.devices.conflicts.resolved_latest_wins
_TOKEN = "2seat-20260801-deadbeef"


class _FakeUiSeat(_FakeSeat):
    """A seat that can read a review list — the rows are handed in."""

    def __init__(self, name, folder, rows):
        super().__init__(name, folder)
        self._rows = rows

    def review_rows(self):
        return self._rows


def test_the_resolved_badge_vocabulary_is_read_off_the_i18n_module():
    """Hard-typing the copy here would let an i18n edit silently turn every
    badge assertion into a tautology (nothing would ever match the allowlist,
    so nothing could ever be 'resolved' — or worse, the reverse)."""
    assert sync_seats.RESOLVED_BADGES == frozenset({"Merged", "Latest kept"})
    assert _MERGED == "Merged" and _LATEST == "Latest kept"


def test_only_this_runs_rows_are_considered():
    """The shared set carries resolved history from every earlier run; an
    assertion about this run must not be reddened by someone else's."""
    rows = [
        (_MERGED, f"set: {_TOKEN}-shared.txt -> abc123"),
        ("concurrent_edit", "set: 2seat-20260731-cafebabe-shared.txt"),
    ]
    assert sync_seats.review_rows_for_run(rows, _TOKEN) == [rows[0]]
    assert sync_seats.unresolved_review_rows(rows, _TOKEN) == []


def test_both_resolution_arms_count_as_resolved():
    rows = [
        (_MERGED, f"set: {_TOKEN}-a.txt -> abc"),
        (_LATEST, f"set: {_TOKEN}-b.txt -> def"),
    ]
    assert sync_seats.unresolved_review_rows(rows, _TOKEN) == []


def test_the_type_badge_of_an_unresolved_row_is_caught():
    """An unresolved row wears a *type* badge ("Concurrent edits" since the
    2026-08-04 copy fix; `conflict_badge_label` still falls through to the raw wire
    string for `concurrent_edit`, which has no i18n key) — either way it
    is not in the RESOLVED allowlist, which is what the fail-closed degrade in
    `auto_resolve_conflict` guarantees this helper can see."""
    for badge in ("Concurrent edits", "concurrent_edit"):
        rows = [(badge, f"set: {_TOKEN}-shared.txt")]
        assert sync_seats.unresolved_review_rows(rows, _TOKEN) == rows


def test_an_unknown_future_badge_fails_closed():
    """The allowlist is the point: a spelling nobody enumerated must go RED,
    never slip through as 'probably fine'."""
    rows = [("Some New Badge", f"set: {_TOKEN}-shared.txt")]
    assert sync_seats.unresolved_review_rows(rows, _TOKEN) == rows


def test_a_disk_only_pair_reports_na_and_asserts_nothing(tmp_path, capsys):
    """`None` (cannot look) must never read as `[]` (looked, saw none)."""
    a = _FakeSeat("a", tmp_path / "a")
    b = _FakeSeat("b", tmp_path / "b")
    a.review_rows = lambda: None
    b.review_rows = lambda: None

    convergence_legs.assert_review_list_agrees([a, b], _TOKEN, 0.0, lambda: "")

    assert "review list: N/A" in capsys.readouterr().out


def test_a_resolved_row_on_a_ui_seat_passes(tmp_path, capsys):
    seat = _FakeUiSeat(
        "a", tmp_path / "a", [(_MERGED, f"set: {_TOKEN}-shared.txt -> abc")]
    )

    convergence_legs.assert_review_list_agrees([seat], _TOKEN, 0.0, lambda: "")

    assert "review list agrees" in capsys.readouterr().out


def test_a_degraded_unresolved_row_reds_and_names_the_degrade(tmp_path):
    seat = _FakeUiSeat(
        "a", tmp_path / "a", [("concurrent_edit", f"set: {_TOKEN}-shared.txt")]
    )

    with pytest.raises(AssertionError) as e:
        convergence_legs.assert_review_list_agrees([seat], _TOKEN, 0.0, lambda: "")

    assert "did NOT auto-resolve" in str(e.value)
    assert "fail-closed degrade" in str(e.value)


def test_zero_rows_is_reported_not_failed(tmp_path, capsys):
    """Convention 14: whether two concurrent edits collide into a *conflict* or
    land as a clean sequential update is a race this test must not bet on. A
    run that produced no conflict says so; it does not red, and it does not
    quietly report coverage it did not get."""
    seat = _FakeUiSeat("a", tmp_path / "a", [])

    convergence_legs.assert_review_list_agrees([seat], _TOKEN, 0.0, lambda: "")

    out = capsys.readouterr().out
    assert "no row for" in out and "clean sequential update" in out


# ── the adopted rescan cadence ───────────────────────────────────────────────


def test_the_adopted_cadence_is_read_off_the_armed_line():
    """`adopted_rescan_ms` reads the field `always_resident::log_rescan_armed`
    writes, in the agent log's own line shape, and nothing else — a DEBUG pipe
    poll or an unrelated `_ms=` field must not read as an armed tick."""
    lines = [
        "2026-09-30T10:00:00.1Z  INFO fauna_sync_engine::always_resident: "
        "folder~1a2b: rescan tick armed rescan_interval_ms=5000",
        "2026-09-30T10:00:00.2Z DEBUG fauna_sync_agent::pipe_server: pipe request ListEngines",
        "2026-09-30T10:00:00.3Z  INFO fauna_sync_engine::engine: debounce_ms=750",
    ]
    assert sync_seats.adopted_rescan_ms(lines) == {5000}


def test_two_folders_armed_at_different_cadences_both_surface():
    """A set, never the first match: a seat whose second folder armed a
    different cadence must fail the `== {want}` pin, not hide behind the first."""
    lines = [
        "INFO x: folder~1: rescan tick armed rescan_interval_ms=5000",
        "INFO x: folder~2: rescan tick armed rescan_interval_ms=300000",
    ]
    assert sync_seats.adopted_rescan_ms(lines) == {5000, 300000}


def test_no_armed_line_reads_as_no_cadence():
    assert sync_seats.adopted_rescan_ms(["INFO x: engine serving"]) == set()


# ── the scenario plan's acts diagnose themselves ─────────────────────────────


def test_a_seat_refusing_the_writers_act_fails_with_every_seats_diagnostics(tmp_path):
    """An act the seat's own folder refuses (on windows, a cfapi root whose
    provider never answered the create's placeholder fetch: `OSError` EINVAL) is
    a failure of THAT seat, and it must carry what an unconverged await carries —
    the step, the writer, the error and every seat's diagnostics with its agent
    log (convention 6). Escaping as a bare `OSError` left the 2026-10-08 windows
    native pair with nothing but a path to go on."""
    from helpers import seat_scenarios

    a, b = _FakeSeat("a", tmp_path / "a"), _FakeSeat("b", tmp_path / "b")

    def refuse(root: Path) -> None:
        raise OSError(22, "Invalid argument", str(root / ".x.tmp"))

    step = seat_scenarios.Step("create", 0, refuse, expect={"x": b"x"})
    note = lambda: "\n".join(s.diagnostics() for s in (a, b))  # noqa: E731

    with pytest.raises(AssertionError) as raised:
        seat_scenarios.run_step(step, [a, b], 0.0, note)

    message = str(raised.value)
    assert "[a] create: the act failed on this seat" in message
    assert "Invalid argument" in message
    assert "seat a: fake" in message and "seat b: fake" in message
