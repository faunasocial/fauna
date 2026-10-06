"""The share plane's app-level two-actor journey — a file authored while the
nest is DOWN arrives on the other user's seat, peer-to-peer (tui, the lead
app).

The tier_1 two-seat test (``fauna-sync-engine/tests/share_pump_two_seats.rs``)
proves the shared pump one layer below the app; THIS test proves the app glue:
two real tui GUIs, each direct-spawning its own private ``fauna-sync-agent``
(the ``FAUNA_E2E_AGENT_PORT``-gated e2e spawn, per-launch XDG isolation), each
binding its contact-plane seat at the ``AccountStoreReady`` edge, advertising
its endpoints on the set's own conversation channel, and pumping.

Choreography (every mutation through the client UI — conventions point 8),
extending ``test_folder_agent_content_sync.py``'s two-seat ground:

1. owner creates a sync set → shares to the member → promotes them to
   **writer** (readers never bind engines) → member accepts the knock;
2. owner binds a folder + writes ``file-1`` → member binds their own folder →
   ``file-1`` arrives **nest-mediated** (anchors: both engines run, the set's
   channel syncs, and the share plane's advertisements have a rail);
3. the member's transfer surface reports an **accepted row** — the BARRIER
   that proves the member cached the owner's advertised address, the peer
   channel admits, AND a row actually crossed; without this, stopping the nest
   below could strand the test un-passably (advertisements ride the
   nest-fetched channel log). ⚠ The presence of a ``share-transfer-item`` row
   does **not** prove that: an admitted dial accepting zero rows mints one
   too, so the barrier reads ``share-transfer-progress``'s counts (2026-08-22
   — the original count-only form passed while the peer path was already
   dead, turning the real defect into a silent step-6 mystery 240 s later);
4. the nest STOPS (``stop_nest`` — the fixture's own proc, never pkill);
5. the owner writes ``file-2`` — the offline own-pending mint (B2.5): the
   pending row serves on the tail page, and the bytes exist ONLY on the
   owner's disk, so arrival at the member is proof of peer transfer;
6. ``file-2`` materializes in the member's bound folder, and the member's
   transfer surface reports the pull honestly.

Device ids: the two fixtures carry DISTINCT ids by construction
(``_E2E_LOGIN_DEVICE_ID`` vs ``_E2E_SHARE_RECIPIENT_DEVICE_ID``) — the
lesson: same-id seats classify each other's rows as their own echo and the
file silently never arrives.
"""

import re
import secrets

import pytest

from common.nest import start_nest_in_place, stop_nest
from conftest import get_available_apps
from helpers.waiting import share_ingest_reading, wait_until
from helpers.folder_content import (
    agent_diagnosis as _agent_diagnosis,
)
from helpers.folder_content import (
    atomic_write as _atomic_write,
)
from helpers.folder_content import (
    await_agent_upload as _await_agent_upload,
)
from helpers.folder_content import (
    bind_location_under_set as _bind_location_under_set,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui leads the plane (`p2p.md` § Cross-user shared-set transfer → build
    # order); linux joined 2026-08-21. Each further
    # leg arrives with its own marker AND an `_SUPPORTED_APPS` entry — a
    # marker alone silently keeps driving tui underneath (the addendum
    # item 9, the same trap on the ceremony's own journey).
    pytest.mark.tui,
    pytest.mark.linux,
    # macOS joined 2026-09-25 as the MEMBER seat of a tui owner (see
    # `_OWNER_APPS`); `--app macos,tui` drives it, `--app macos` alone has no
    # owner to pair with and collects nothing.
    pytest.mark.macos,
    # macOS' production spawner is launchd, which an e2e launch must not
    # bootstrap; this marker is what makes its launch construct the private
    # child-agent spawner instead, and the plane's provisioner (its two agent
    # verbs) is built from that spawner — no agent, no provisioner, no plane.
    # A no-op for tui/linux, which direct-spawn their agent already.
    pytest.mark.real_sync_agent,
]

#: Apps with a landed share-plane leg. Grow this — and the app's own marker
#: above — together.
_SUPPORTED_APPS = ("tui", "linux", "macos")

#: The apps that can be the OWNER seat of this journey: the choreography has
#: the owner promote the member to **writer** (readers never bind engines),
#: which needs `folder-member-role-select`. macOS has no owner-side
#: promote-to-writer UI yet — the id is declared debt there — so it takes the
#: MEMBER seat of a tui owner instead, the same pairing
#: `test_macos_writer_member_decrypts_owner_upload` already runs on macOS. Grow
#: this the day macOS (or another leg) gains the control, and its own leg is
#: then also proven as the SERVING seat.
_OWNER_APPS = ("tui", "linux")

#: The owner a supported app that cannot own drives its member seat against —
#: the lead app, which is also the one macOS can host next to macOS.
_LEAD_OWNER_APP = "tui"


def _share_plane_seat_pairs():
    """One (owner, member) pair per supported app that is drivable HERE.

    An app that can own runs BOTH seats deliberately: the journey proves one
    app's glue against itself (bind → advertise → sink → pull), and a full
    cross product would multiply a 20-minute two-GUI journey by the square of
    the app count for combinations no leg owes. An app that cannot own
    (`_OWNER_APPS`) is the member of a tui owner, and needs tui present too.
    """
    available = get_available_apps()
    pairs = []
    for app in _SUPPORTED_APPS:
        if app not in available:
            continue
        if app in _OWNER_APPS:
            pairs.append((app, app))
        elif _LEAD_OWNER_APP in available:
            pairs.append((_LEAD_OWNER_APP, app))
    return pairs

# The scan cadence is no per-folder choice since phase 5 (2026-08-20): every
# e2e launch ticks at the harness's 30 s `FAUNA_E2E_RESCAN_MS` default
# (`drivers/tui.py` / `drivers/linux.py`; the compile-gated seam is
# `always_resident::rescan_interval`), so a scan-driven path still fires inside
# the poll windows below — the cadence the retired wizard picker used to set.

# ── Named budgets (conventions point 14: generous ceilings, deadline polls) ──
# Nest-mediated hydration crosses two agents + a nest round-trip on rescan
# cadence (the content-sync twin's number).
_NEST_HYDRATION_S = 360.0
# The peer barrier: owner's glue must bind + advertise (pump tick = 5s via
# FAUNA_SHARE_PUMP_SECS), the advertisement must ride the 2s conversation poll
# to the member, the member's sink writes the dial row, and the member's next
# pump pass dials + admits + reports. Each link is seconds; the ceiling is not.
_PEER_BARRIER_S = 240.0
# Peer-side arrival while the nest is down: pump tick + one dial + a paged
# pull + spool + ingest of a small file.
_PEER_TRANSFER_S = 240.0


def _read_or_none(path):
    try:
        return path.read_text()
    except (FileNotFoundError, OSError):
        return None


def _await_member_file(member_folder, filename, expected, member_app):
    """Deadline-poll until the member's bound folder holds ``filename`` ==
    ``expected`` (convention 14 — the shared ``wait_until``)."""
    wait_until(
        lambda: _read_or_none(member_folder / filename) == expected,
        _PEER_TRANSFER_S,
        interval=1.0,
        diagnose=lambda: (
            f"[member] {filename}: "
            + (
                "NEVER ARRIVED"
                if _read_or_none(member_folder / filename) is None
                else f"content mismatch: got {_read_or_none(member_folder / filename)!r}"
            )
            + " with the nest DOWN — the peer pull did not deliver it. The "
            "owner's engine minted it offline (pending row + local plaintext), "
            "so the failing links are: the member's cached dial row, the "
            "dial/admission, the tail-page pull, or the ingest door. "
            f"app error: {member_app.error_text()!r}\n"
            + _agent_diagnosis(member_app, "member")
        ),
    )


def _share_and_reach_peer_barrier(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """Steps 1–3, shared by every nest-down journey in this module: share the
    set to the member as a writer, hydrate one file nest-mediated, and wait
    until the member has ACCEPTED a row peer-side — the barrier that makes
    stopping the nest safe.

    Returns ``(owner_app, member_app, nest, owner_folder, member_folder,
    set_name)``.
    """
    from tests.api import conv_api

    owner_app, nest, _owner = folder_share_owner_app
    member_app, _nest2, member = folder_share_recipient_app

    # Both seats' agent logs on EVERY outcome (a pytest-timeout kill unwinds
    # through no pytest.fail) — the content-sync twin's posture.
    request.addfinalizer(lambda: print(_agent_diagnosis(member_app, "member")))
    request.addfinalizer(lambda: print(_agent_diagnosis(owner_app, "owner")))

    # ── 1. share + writer promotion + accept (the content-sync choreography) ──
    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
        30,
        interval=1.0,
        diagnose=lambda: (
            "the member never published a fetchable KeyPackage, so the owner's "
            "share cannot admit them to the set's MLS group"
        ),
    )

    set_name = f"peerpull-{secrets.token_hex(4)}"
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)
    owner_row = ob.find_and_expand_folder(set_name)
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])

    wait_until(
        lambda: ob.shared_member_count() == 1,
        20,
        interval=0.5,
        diagnose=lambda: (
            "the share should land exactly one member; "
            f"error={owner_app.error_text()!r}"
        ),
    )

    ob.set_member_access("writer", 0, row=owner_row)
    wait_until(
        lambda: ob.member_access(0, row=owner_row) == "writer",
        15,
        interval=0.5,
        diagnose=lambda: (
            "the member must persist as writer — a reader never binds an "
            "engine, so nothing downstream could run; "
            f"error={owner_app.error_text()!r}"
        ),
    )

    mb = member_app.backups
    mb.navigate_folders()
    mb.wait_for_pending_shares(1)
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(0) == 0, (
        f"[member] accepting should consume the knock; "
        f"error={member_app.error_text()!r}"
    )

    # The folder list re-fetches on page-VISIBLE, so the predicate toggles
    # away and back after each miss (the proven pattern in the content-sync
    # twin) — polling without the toggle reads an empty list forever.
    def _accepted_set_listed():
        titles = [mb.folder_title(i) for i in range(mb.folder_count())]
        if any(set_name in title for title in titles):
            return True
        mb.navigate_devices()
        mb.navigate_folders()
        return False

    wait_until(
        _accepted_set_listed,
        90,
        interval=1.0,
        diagnose=lambda: (
            f"[member] the accepted set {set_name!r} never appeared in the "
            f"folder list within 90s; error={member_app.error_text()!r}\n"
            + _agent_diagnosis(member_app, "member")
        ),
    )

    # ── 2. nest-mediated hydration (the anchor both later steps stand on) ──
    owner_folder = tmp_path / "owner-bound"
    owner_folder.mkdir()
    _bind_location_under_set(owner_app, set_name, owner_folder, seat="owner")

    file_1 = f"online-{secrets.token_hex(3)}.txt"
    payload_1 = f"nest-mediated — {secrets.token_hex(8)}\n"
    _atomic_write(owner_folder / file_1, payload_1)
    _await_agent_upload(owner_app, file_1, seat="owner")

    member_folder = tmp_path / "member-bound"
    member_folder.mkdir()
    _bind_location_under_set(member_app, set_name, member_folder, seat="member")

    wait_until(
        lambda: _read_or_none(member_folder / file_1) == payload_1,
        _NEST_HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[member] {file_1} never hydrated NEST-MEDIATED within "
            f"{_NEST_HYDRATION_S:.0f}s — the ordinary path is broken, so the "
            f"peer-pull half below cannot be read as a peer result. "
            f"error={member_app.error_text()!r}\n"
            + _agent_diagnosis(member_app, "member")
        ),
    )

    # ── 3. the peer barrier: the member has pulled from the owner PEER-SIDE ──
    # Also the moment the surface's six ids are first proven live in a real
    # app. The folders page repaints per frame, so a plain poll reads fresh
    # state.
    #
    # ⚠ **The barrier requires an ACCEPTED ROW, not merely an item row.**
    # Counting `share-transfer-item` was this barrier's original form and it
    # could not do the job it was added for. `pull_set_from_peer` returns
    # `Ok(outcome)` for a dial that is admitted and accepts ZERO rows;
    # `pull_pass` pushes every such outcome into the cell, and `update_cell`
    # mints one item row per outcome — so the row count passes on **dial +
    # admission alone**, with the peer path never having moved anything.
    # (Pinned at tier_1 by
    # `an_uncached_writer_is_refused_wholesale_and_looks_like_a_quiet_pass`.)
    # That is exactly how a member refusing every row for an empty writer
    # roster sailed through here and failed 240 s later at step 6 as a silent
    # "NEVER ARRIVED", when the peer path had in fact been dead since step 3.
    #
    # `share-transfer-progress` carries the outcome's own
    # `materialized`/`rows_accepted` counts, so a non-zero reading is real
    # evidence that a row crossed. This is a STRENGTHENING — the barrier now
    # guarantees what its own docstring always claimed, which is what makes
    # stopping the nest below safe.
    md = member_app.driver
    mb.navigate_folders()

    def _transfer_counts(index: int) -> tuple[int, int]:
        """`(files, rows)` off one item's progress reading.

        The text is the shared `folders.share_transfer_progress`
        (`'{files} file(s), {rows} change(s) this pass'`), so the digits are
        read positionally rather than by matching the prose — the wording is
        i18n's to change, the two counts are the contract.
        """
        text = md.get_text("share-transfer-progress", scope=f"share-transfer-item[{index}]")
        nums = re.findall(r"\d+", text or "")
        if len(nums) < 2:
            return (0, 0)
        return (int(nums[0]), int(nums[1]))

    def _member_pulled_peer_side():
        if not md.is_visible("share-serve-status"):
            return False
        for k in range(md.count("share-transfer-item")):
            files, rows = _transfer_counts(k)
            if files > 0 or rows > 0:
                return True
        return False

    def _barrier_diagnosis():
        status = (
            (md.get_text("share-serve-status") or "")
            if md.is_visible("share-serve-status")
            else "<absent>"
        )
        items = md.count("share-transfer-item")
        readings = ", ".join(
            f"[{k}] files={f} rows={r}" for k, (f, r) in
            ((k, _transfer_counts(k)) for k in range(items))
        ) or "<no item rows>"
        return (
            "[member] no peer row was ACCEPTED within "
            f"{_PEER_BARRIER_S:.0f}s (share-serve-status={status!r}; "
            f"{items} item row(s): {readings}). "
            "The peer plane never moved a row while the nest was UP, so "
            "stopping the nest would strand the test: either a seat never bound "
            "(brake/AccountStoreReady), the owner never advertised, the "
            "member's sink never wrote the dial row, the dial/admission "
            "failed, or the member REFUSED the rows it was served. "
            "⚠ Item rows present with files=0 rows=0 is the last of those: a "
            "dial that is admitted and accepts nothing still mints an item "
            "row, and an empty cached writer roster refuses every row before "
            "weighing it (look for 'share pump: peer rows refused' and "
            "'share writer roster: control plane not connected' below). "
            f"error={member_app.error_text()!r}\n"
            # Which of the five candidates above actually fired, read off the
            # apps' own ingest tallies instead of inferred from their logs.
            # `0 item row(s)` and `item rows reading files=0 rows=0` already
            # mean different things here; this says which side of the SINK the
            # advertisement died on, per seat, which is the split the sentence
            # above can only enumerate.
            + share_ingest_reading(md, "member")
            + "\n"
            + share_ingest_reading(owner_app.driver, "owner")
            + "\n"
            + _agent_diagnosis(member_app, "member")
            + "\n"
            + _agent_diagnosis(owner_app, "owner")
        )

    wait_until(
        _member_pulled_peer_side,
        _PEER_BARRIER_S,
        interval=2.0,
        diagnose=_barrier_diagnosis,
    )

    # The owner's serve side must say so too (rule-5 transparency).
    od = owner_app.driver
    ob.navigate_folders()
    assert od.is_visible("share-serve-status"), (
        "[owner] the serve-status line must render once the share glue's cell "
        f"exists: {od.diagnose('share-serve-status')}"
    )
    owner_status = od.get_text("share-serve-status") or ""
    assert "off" not in owner_status.lower(), (
        f"[owner] the brake must not read as off while serving: {owner_status!r}"
    )

    return owner_app, member_app, nest, owner_folder, member_folder, set_name


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    _share_plane_seat_pairs(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (point 9: bounded always): two GUI apps + two real agents +
# a real MLS share + one nest-mediated hydration + a nest stop + a peer pull.
# A ceiling, not an expectation.
@pytest.mark.timeout(1800)
@pytest.mark.feature("offline-sharing")
def test_member_pulls_offline_authored_file_from_owner_peer(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """A file authored with the nest down arrives peer-to-peer, and the
    transfer surface reports it (rule-5/Dim-3 honesty)."""
    owner_app, member_app, nest, owner_folder, member_folder, _set_name = (
        _share_and_reach_peer_barrier(
            request, folder_share_owner_app, folder_share_recipient_app, tmp_path
        )
    )
    mb = member_app.backups
    md = member_app.driver

    # ── 4–6. nest DOWN → owner authors → member pulls peer-side ──
    try:
        stop_nest(nest, graceful=True)

        file_2 = f"offline-{secrets.token_hex(3)}.txt"
        payload_2 = f"authored in the cabin — {secrets.token_hex(8)}\n"
        _atomic_write(owner_folder / file_2, payload_2)

        _await_member_file(member_folder, file_2, payload_2, member_app)

        # The surface reports the pull honestly: an item row for this set with
        # a non-refusal state (Dim-3 — a gate refusal would name its tier).
        mb.navigate_folders()
        assert md.count("share-transfer-item") >= 1, (
            "[member] the transfer surface lost its rows after the peer pull "
            f"that just delivered {file_2}"
        )
        state = md.get_text("share-transfer-state") or ""
        assert "limited" not in state.lower(), (
            f"[member] the transfer gate refused a pull this test needed: {state!r}"
        )
    finally:
        # The session-scoped nest must outlive this test healthy — same port,
        # same data dir, no factory reset.
        start_nest_in_place(nest)

    assert not member_app.has_error(), (
        f"[member] the peer pull surfaced an error: {member_app.error_text()!r}"
    )


#: Apps whose seat publishes the serve tally and takes the serve hold
#: (`offline_share_hold_serves`, `share_serve_tally`) — the OWNER seat's
#: doors, so this reads the owner app. macOS binds the plane as a member
#: (`_SUPPORTED_APPS`) but has not wired the three FFI test doors into its own
#: agent dispatcher, and could not be the owner here yet (`_OWNER_APPS`); it
#: joins this and `_PROBE_APPS` together with that. The remaining native legs
#: (windows, iOS, android) join once they bind the plane at all.
_SERVE_HOLD_APPS = ("tui", "linux")


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    _share_plane_seat_pairs(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (point 9): the step 1–3 ground above plus three peer pulls.
@pytest.mark.timeout(1800)
@pytest.mark.feature("offline-sharing")
def test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """`offline-sharing` outcome 8: *an interrupted device-to-device transfer
    picks up where it stopped without re-sending what arrived* (`p2p.md`
    § Cross-user shared-set transfer — "with held chunks never re-sent").

    "Never re-sent" is a fact about the wire, so the SENDER witnesses it: the
    owner's seat counts every manifest and chunk it answers, per path
    (`share_serve_tally`). The member re-lists every offline-authored file on
    every pass (a pending row serves on the tail page until the nest sequences
    it), so a member that did not know what it already held would ask for it
    again, and the count would read 2.

    The interruption is a STATE, not a race (convention 14). With the serve
    hold on, the member's next manifest request parks unanswered on the owner,
    so the transfer is provably in flight. The journey cuts the connection
    there, then lifts the hold, and the parked request fails uncounted.

    1. nest down; the owner authors ``a``; it arrives whole (what "arrived"
       means below);
    2. hold on; the owner authors ``b`` and ``c``; the member's request parks;
    3. the owner drops the member's connection mid-transfer; hold off;
    4. ``b`` and ``c`` arrive on a later pass;
    5. the owner answered each of ``a``, ``b`` and ``c`` exactly once, and
       ``a``, which arrived before the cut, was never asked for again.
    """
    from helpers.app_surface import app_name, skip_unbuilt

    if app_name(folder_share_owner_app[0].driver) not in _SERVE_HOLD_APPS:
        skip_unbuilt(
            folder_share_owner_app[0].driver,
            surface="offline_share_hold_serves / share_serve_tally",
            detail="the serve hold and the serve tally have no agent arm on this app",
            tracked="p2p.md § Implementation status today (the resumed-transfer paragraph)",
        )

    owner_app, member_app, nest, owner_folder, member_folder, _set_name = (
        _share_and_reach_peer_barrier(
            request, folder_share_owner_app, folder_share_recipient_app, tmp_path
        )
    )
    ob = owner_app.backups

    def _tally_line():
        try:
            return f"owner serve tally: {ob.share_serve_tally()!r}"
        except Exception as exc:  # pragma: no cover - diagnostic path only
            return f"owner serve tally unavailable: {exc!r}"

    try:
        stop_nest(nest, graceful=True)

        # ── 1. `a` arrives whole before anything is interrupted ──
        tag = secrets.token_hex(3)
        file_a, file_b, file_c = (f"resume-{n}-{tag}.txt" for n in ("a", "b", "c"))
        payloads = {
            name: f"{name} — authored with the nest down — {secrets.token_hex(8)}\n"
            for name in (file_a, file_b, file_c)
        }
        _atomic_write(owner_folder / file_a, payloads[file_a])
        _await_member_file(member_folder, file_a, payloads[file_a], member_app)

        # ── 2. hold on; the next manifest request parks mid-transfer ──
        ob.hold_share_serves(True)
        try:
            _atomic_write(owner_folder / file_b, payloads[file_b])
            _atomic_write(owner_folder / file_c, payloads[file_c])
            wait_until(
                lambda: ob.share_serve_tally().get("parked", 0) >= 1,
                _PEER_TRANSFER_S,
                interval=1.0,
                diagnose=lambda: (
                    "[owner] no manifest request parked on the serve hold, so no "
                    "transfer of the files authored after it ever began: the "
                    "member never re-dialed, or never listed the new pending "
                    f"rows. {_tally_line()}\n"
                    + _agent_diagnosis(member_app, "member")
                ),
            )
            # Nothing authored after the hold can have crossed: every body
            # starts with a manifest answer, and those are parked.
            arrived_early = [
                n for n in (file_b, file_c) if _read_or_none(member_folder / n) is not None
            ]
            assert not arrived_early, (
                f"[member] {arrived_early} arrived while every manifest answer was "
                f"held — the hold did not hold. {_tally_line()}"
            )

            # ── 3. cut the connection under the parked request ──
            dropped = ob.drop_offline_share_connections()
            assert dropped >= 1, (
                "[owner] a manifest request was parked, so the member's connection "
                f"must have been open to drop; dropped={dropped}. {_tally_line()}"
            )
        finally:
            ob.hold_share_serves(False)

        # ── 4. the transfer picks up on a later pass ──
        _await_member_file(member_folder, file_b, payloads[file_b], member_app)
        _await_member_file(member_folder, file_c, payloads[file_c], member_app)

        # ── 5. nothing that arrived was sent again ──
        tally = ob.share_serve_tally()
        assert tally.get("parked", 0) == 0 and not tally.get("held"), (
            f"[owner] the hold must be lifted and drained: {tally!r}"
        )
        for name in (file_a, file_b, file_c):
            served = tally.get("manifests", {}).get(name, 0)
            chunks = tally.get("chunks", {}).get(name, 0)
            assert (served, chunks) == (1, 1), (
                f"[owner] {name} was answered {served} manifest(s) and {chunks} "
                "chunk(s); every file here is one small chunk, so anything but "
                "(1, 1) means a body was sent again after it had arrived — or, "
                f"for 0, never crossed the wire from this seat at all. {tally!r}"
            )
    finally:
        # A process-wide hold must never outlive the journey, and the
        # session-scoped nest must come back healthy on the same port.
        try:
            owner_app.backups.hold_share_serves(False)
        finally:
            start_nest_in_place(nest)

    assert not member_app.has_error(), (
        f"[member] the resumed pull surfaced an error: {member_app.error_text()!r}"
    )


#: Apps whose seat carries the share probe (`offline_share_probe_set`, one
#: shared call: `share_probe::probe_set_from_args`). Same joining rule as
#: `_SERVE_HOLD_APPS`.
_PROBE_APPS = ("tui", "linux")


def _share_plane_seat_trios():
    """(owner, member, stranger) on one app, for the apps with the probe.

    Same-app pairs only: a mixed pair (a member-only app under a tui owner)
    would otherwise re-yield tui's own trio, the probe being a property of the
    OWNER's and stranger's seats, not the member's.
    """
    return [
        (owner, member, owner)
        for owner, member in _share_plane_seat_pairs()
        if owner == member and owner in _PROBE_APPS
    ]


def _files_holding(root, needle: bytes) -> list[str]:
    """Every file under ``root`` whose bytes contain ``needle``."""
    import os

    hits = []
    for dirpath, _dirs, files in os.walk(root):
        for name in files:
            path = os.path.join(dirpath, name)
            try:
                with open(path, "rb") as fh:
                    if needle in fh.read():
                        hits.append(path)
            except OSError:
                continue
    return hits


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app,folder_share_stranger_app",
    _share_plane_seat_trios(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (point 9): the step 1–3 ground above, a third app, two probes.
@pytest.mark.timeout(1800)
@pytest.mark.feature("offline-sharing")
def test_a_person_the_folder_was_never_shared_with_gets_nothing_readable(
    request,
    folder_share_owner_app,
    folder_share_recipient_app,
    folder_share_stranger_app,
    tmp_path,
):
    """`offline-sharing` outcome 7: *a person the folder was never shared with
    gets nothing readable from your device* (`p2p.md` § Cross-user shared-set
    transfer — admission is the front door, the M2 seal is the wall).

    No app gesture makes a stranger pull a set — the pump dials only sets its
    own seat belongs to — so the stranger's side is the share probe
    (`offline_share_probe_set`): the request a hostile client would send. It
    dials the owner by the compare code the owner shows, claims the set, and
    then asks for the set's rows and manifests WHATEVER the admission said.

    The probe is its own control. Run first from the member's seat, against
    the same owner and set, it must be admitted, list the payload's row and
    fetch its manifest. Only the identity differs between the two runs, so the
    stranger's empty report cannot be a probe that never worked. The stranger
    is then handed the very manifest hashes the member saw.

    "Nothing readable" is asserted three ways: the probe got no rows, no
    paths and no manifest bodies; the owner's serve tally did not move for the
    payload; and the payload's bytes appear nowhere under the stranger's
    per-launch state root.
    """
    from tests.api import conv_api

    owner_app, member_app, nest, owner_folder, member_folder, set_name = (
        _share_and_reach_peer_barrier(
            request, folder_share_owner_app, folder_share_recipient_app, tmp_path
        )
    )
    stranger_app, _nest3, stranger = folder_share_stranger_app
    _owner_app, _nest1, owner = folder_share_owner_app
    ob, mb, sb = owner_app.backups, member_app.backups, stranger_app.backups

    # The payload: authored now, and arrived at the member, so the owner holds
    # it and a member can be served it.
    secret = secrets.token_hex(16)
    file_s = f"never-for-strangers-{secrets.token_hex(3)}.txt"
    payload = f"for members only — {secret}\n"
    _atomic_write(owner_folder / file_s, payload)
    _await_member_file(member_folder, file_s, payload, member_app)

    group_id_hex = conv_api.folder_group_id_hex(nest["port"], owner, set_name)
    assert group_id_hex, f"[owner] the set {set_name!r} has no MLS group id on the nest"

    # The compare code the owner's seat shows is its address; the seat is the
    # one the share plane already bound, so opening the panel binds nothing.
    ob.navigate_folders()
    ob.open_offline_share()
    owner_code = ob.offline_share_own_code()
    ob.cancel_offline_share()
    assert owner_code, "[owner] the co-present panel showed no compare code to dial"

    # ── the control: the member's probe is admitted and served ──
    member_report = mb.probe_shared_set(owner_code, group_id_hex)
    assert member_report.get("dialed") and member_report.get("admitted"), (
        f"[member] the control probe was not admitted by the owner — the probe "
        f"cannot witness a refusal if it cannot witness an admission: {member_report!r}"
    )
    assert file_s in member_report.get("paths", []), (
        f"[member] the control probe did not list the payload's row: {member_report!r}"
    )
    assert member_report.get("manifests", 0) >= 1 and member_report.get("manifest_hashes"), (
        f"[member] the control probe fetched no manifest: {member_report!r}"
    )

    # ── the stranger: bind its listener the way a person would, then probe ──
    sb.navigate_folders()
    assert sb.offline_share_available(), (
        "[stranger] the co-present affordance is absent, so its listener cannot "
        f"bind; error={stranger_app.error_text()!r}"
    )
    sb.open_offline_receive()

    before = ob.share_serve_tally()
    stranger_report = sb.probe_shared_set(
        owner_code, group_id_hex, member_report["manifest_hashes"]
    )
    after = ob.share_serve_tally()

    assert stranger_report.get("dialed"), (
        "[stranger] the probe never reached the owner's listener, so it proves "
        f"nothing about the serve door: {stranger_report!r}"
    )
    assert not stranger_report.get("admitted") and stranger_report.get("admit_error"), (
        f"[stranger] the owner admitted a non-member: {stranger_report!r}"
    )
    assert stranger_report.get("rows", 0) == 0 and not stranger_report.get("paths"), (
        f"[stranger] the owner served change rows to a non-member: {stranger_report!r}"
    )
    assert stranger_report.get("rows_error"), (
        f"[stranger] the row request must be refused, not answered empty: {stranger_report!r}"
    )
    assert stranger_report.get("manifests", 0) == 0, (
        f"[stranger] the owner served manifest bodies to a non-member: {stranger_report!r}"
    )
    assert len(stranger_report.get("manifest_errors", [])) == len(
        member_report["manifest_hashes"]
    ), f"[stranger] every manifest request must be refused: {stranger_report!r}"

    for kind in ("manifests", "chunks"):
        was = before.get(kind, {}).get(file_s, 0)
        now = after.get(kind, {}).get(file_s, 0)
        assert now == was, (
            f"[owner] the serve tally's {kind} for {file_s} moved {was} -> {now} "
            f"across the stranger's probe — something was served to a non-member. "
            f"before={before!r} after={after!r}"
        )

    stranger_driver = stranger_app.driver
    roots = [stranger_driver._tmp_dir]
    store_root = getattr(stranger_driver, "_resolved_store_root", None)
    if store_root and not str(store_root).startswith(str(stranger_driver._tmp_dir)):
        roots.append(store_root)
    leaked = [hit for root in roots for hit in _files_holding(root, secret.encode())]
    assert not leaked, (
        f"[stranger] the payload's plaintext rests on the stranger's device: {leaked}"
    )

    assert not stranger_app.has_error(), (
        f"[stranger] the probe surfaced an app error: {stranger_app.error_text()!r}"
    )
