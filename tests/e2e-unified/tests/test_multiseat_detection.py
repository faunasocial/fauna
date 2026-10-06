"""tier_1 gates for the multiseat harness's DETECTION logic.

The three-machine live run itself is opt-in and needs a live nest, but the part
that decides *what a missing file means* is pure arithmetic over two inputs (the
bound folder + the nest listing) and belongs in a fast, always-run tier.

Why this file exists at all: on 2026-07-24 run ``20260724-06`` the macOS seat
printed ``cohort converged — PASS`` while its own ``-06-done-macos.txt`` had
never reached the nest, and BOTH peers failed waiting for it. Phases 1-4 build
their ``expect`` set from PEERS ONLY, so no seat ever checks its own uploads —
the green seat was the broken one. The same peers-only blind spot had already
misdirected three sessions at the wrong machine earlier that day. These tests
pin the fix so it cannot silently regress:

  * a seat that did not upload FAILS instead of printing PASS, and
  * a missing file is diagnosed against the NEST, never as a bare
    "NEVER ARRIVED" (which states a local fact in global-sounding words).

Soundness invariant under test: presence in the listing is sound on any client,
but ABSENCE is only sound from a COMPLETE read — a lazy-list GUI app
under-registers rows. A lossy read must degrade to UNCERTIFIED, never to a red.
"""

from __future__ import annotations

import pytest

import tests.test_filesync_multiseat_live as ms

pytestmark = [pytest.mark.tier_1]


RUN = "20260724-06"


@pytest.fixture(autouse=True)
def _fast_and_deterministic(monkeypatch):
    """Pin the run id — these tests exercise the decision logic, not the clock."""
    monkeypatch.setattr(ms, "RUN_ID", RUN)
    monkeypatch.setattr(ms, "WINDOW", 0.05)


def _nest(*names: str):
    return set(names)


def _all_mine(seat: str) -> set[str]:
    return {
        f"{RUN}-hello-{seat}.txt",
        f"{RUN}-ready-{seat}.txt",
        f"{RUN}-done-{seat}.txt",
    }


# ── file authorship ──────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "name,expected",
    [
        (f"{RUN}-done-macos.txt", "macos"),
        (f"{RUN}-hello-linux.txt", "linux"),
        (f"{RUN}-ready-windows.txt", "windows"),
        (f"{RUN}-shared.txt", None),  # every seat edits it; no single author
    ],
)
def test_run_file_author_identifies_the_owning_seat(name, expected):
    assert ms._run_file_author(name) == expected


# ── absence diagnosis: the three genuinely different cases ───────────────────


def test_absent_but_on_the_nest_blames_the_download_not_the_author():
    """The file IS on the nest, so the author uploaded fine — this seat's
    download is the broken side. Blaming the author here is the misdirection."""
    name = f"{RUN}-done-macos.txt"
    msg = ms._absent_diagnosis(name, "linux", _nest(name), complete=True)
    assert "ON THE NEST" in msg
    assert "nest -> linux is the broken direction" in msg
    assert "NOT the suspect" in msg


def test_absent_and_not_on_a_complete_nest_read_blames_the_author():
    """A complete read that lacks the file is a real verdict: the author never
    uploaded it. This is mac's case in run 20260724-06."""
    msg = ms._absent_diagnosis(
        f"{RUN}-done-macos.txt", "linux", _nest(f"{RUN}-done-linux.txt"), complete=True
    )
    assert "NOT ON THE NEST" in msg
    assert "macos -> nest is the broken direction" in msg


def test_absent_under_a_lossy_read_refuses_to_conclude():
    """A lazy-list client under-registers rows, so absence proves nothing. The
    wording must stay hedged or it manufactures a false accusation."""
    msg = ms._absent_diagnosis(f"{RUN}-done-macos.txt", "linux", _nest(), complete=False)
    assert "cannot prove absence" in msg
    assert "NOT ON THE NEST" not in msg


def test_absent_with_an_unreadable_nest_names_no_machine():
    msg = ms._absent_diagnosis(f"{RUN}-done-macos.txt", "linux", None, complete=False)
    assert "UNDETERMINED" in msg
    assert "do not blame any machine" in msg


def test_no_diagnosis_is_ever_the_bare_never_arrived_wording():
    """The flat phrasing is the bug: it states "not in my folder" in words that
    sound like "the peer never wrote it"."""
    name = f"{RUN}-done-macos.txt"
    for nest, complete in [(_nest(name), True), (_nest(), True), (_nest(), False), (None, False)]:
        msg = ms._absent_diagnosis(name, "linux", nest, complete)
        assert "NEVER ARRIVED on this seat" not in msg


# ── self-check: am I the broken seat? ────────────────────────────────────────


def _folder_with(tmp_path, *names: str):
    for n in names:
        (tmp_path / n).write_text("x")
    return tmp_path


def test_self_check_clears_this_seat_when_its_files_are_on_the_nest(tmp_path):
    folder = _folder_with(tmp_path, f"{RUN}-hello-linux.txt", f"{RUN}-done-linux.txt")
    note = ms._own_upload_note(folder, "linux", _all_mine("linux"), complete=True)
    assert "MY OWN files DID reach the nest" in note
    assert "upload path is healthy" in note


def test_self_check_names_this_seat_as_the_broken_uploader(tmp_path):
    """The line that would have redirected three sessions to the right machine."""
    folder = _folder_with(tmp_path, f"{RUN}-hello-macos.txt", f"{RUN}-done-macos.txt")
    note = ms._own_upload_note(
        folder, "macos", _nest(f"{RUN}-hello-macos.txt"), complete=True
    )
    assert "NEVER REACHED THE NEST" in note
    assert f"{RUN}-done-macos.txt" in note
    assert "any peer reporting these as missing is CORRECT" in note


def test_self_check_stays_hedged_under_a_lossy_read(tmp_path):
    folder = _folder_with(tmp_path, f"{RUN}-done-macos.txt")
    note = ms._own_upload_note(folder, "macos", _nest(), complete=False)
    assert "possibly" in note
    assert "NEVER REACHED THE NEST" not in note


def test_self_check_reports_unknown_when_the_nest_is_unreadable(tmp_path):
    folder = _folder_with(tmp_path, f"{RUN}-done-macos.txt")
    note = ms._own_upload_note(folder, "macos", None, complete=False)
    assert "UNKNOWN" in note


def test_self_check_only_considers_files_this_seat_actually_wrote(tmp_path):
    """At phase 1 only the hello exists; ready/done are not yet owed and must not
    be reported missing."""
    folder = _folder_with(tmp_path, f"{RUN}-hello-linux.txt")
    note = ms._own_upload_note(
        folder, "linux", _nest(f"{RUN}-hello-linux.txt"), complete=True
    )
    assert "DID reach the nest" in note
    assert "ready" not in note and "done" not in note


def test_self_check_ignores_peer_files_sitting_in_my_folder(tmp_path):
    """Peers' files land here by design; only MY OWN uploads are my problem."""
    folder = _folder_with(
        tmp_path, f"{RUN}-hello-linux.txt", f"{RUN}-done-macos.txt"
    )
    note = ms._own_upload_note(
        folder, "linux", _nest(f"{RUN}-hello-linux.txt"), complete=True
    )
    assert "DID reach the nest" in note
    assert "done-macos" not in note
