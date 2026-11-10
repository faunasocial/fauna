"""tier_3 e2e: after the owner takes their account back, a member still opens
the folder's files from before, and a file the stolen key writes afterwards is
refused.

Outcome 23 of ``docs/features/share-a-folder.md``;
``docs/goal/architecture/succession-cut.md`` § Implementation status today (the
reader (c) — rows signed under a retired identity of the set's lineage stay
admissible as history) and ``writer-signed-change-records.md`` ruling (8)
(a succession moves the nest's stamp and no signed byte, so a pre-recovery row
is served naming the successor and the reader recovers the signed actor through
the proven predecessor). Every link is pinned in Rust
(``sync_row_verify.rs``); what no Rust pin reaches is a member's real app
reading a real nest after a real ceremony.

**The three parties** (the same cast as
``test_identity_succession_shared_folder_rekey.py``, outcome 35 of
take-your-account-back, whose share gesture this file reuses):

- **A** — the owner (``succeedable_app``): creates a set, binds a folder, adds
  a file, shares the set to M, then takes the account back.
- **M** — a member's real-engine seat (``succession_member_app``).
- **S** — a second seat of A's OLD seed, bound to the set before the ceremony
  and left running through it: what a seed thief holds.

**The observables.**

1. *A member still opens the files from before.* M's Media page is a projection
   reader that judges the set's listing whole on every refresh
   (``writer-signed-change-records.md`` (8)(f): "a listing is judged whole on
   every refresh"), so a re-entry AFTER the ceremony re-judges A's pre-recovery
   row — signed by the retired identity, served naming the successor — and the
   download produces A's plaintext. Its control: the same open BEFORE the
   ceremony.
2. *A file the stolen key writes afterwards is refused.* S drops a file into
   its bound folder after the ceremony; the nest holds no change row of it and
   it never reaches M, while a file the successor adds after it does reach M.
   Its control: a file S added BEFORE the ceremony did reach M, so S's seat
   genuinely could write.

⚠ **The owner holds its recovery kit BEFORE the share, and that order is the
honest one.** A member verifies the in-group succession statement only against
a chain head it learned independently, before the ceremony — its peer-anchor
harvest of the folder's owner, which runs once per session soon after it joins
(``identity-succession.md`` § The succession statement → *the peer-profile
harvest*). An owner with no kit publishes no recovery head, so that harvest
settles empty; a kit minted after it and spent seconds later is, to every
member, exactly what a seed thief minting a kit and "succeeding" looks like, so
the witness refuses it and the folder commit walk, its harvest already settled,
does not hold — the successor's remove-old is refused and the member's set
channel forks (``federation-shared-folders.md`` § Cross-nest shared folders +
channel append → *The folder commit walk inherits the harvest wait*, the bound).
The first run of this file minted the kit after the share and measured exactly
that fork.

**The second test runs that fork on purpose and heals it**
(``federation-shared-folders.md`` § Cross-nest shared folders + channel append
→ *A forked seat is final, says so, and heals only by a fresh Welcome*): the
forked member's badge reads the third text, the successor's member list stops
naming it, and the successor's ordinary share gesture delivers a fresh Welcome
that replaces the seat — the successor's later file then reaches the member,
whose earlier files stay. No relaunch: the re-invite rides the live drain.

Each seat adds and reads the set's files through its own door
(``helpers/succession_file_door.py``): a bound local folder on tui and linux,
the Media page's upload and download on web, which keeps no bound folder — the
assertions are the same on every app. The owner's first file is added after the
share, so on every door it is sealed under the set's content key.

Convention 8: the set, the share, the binds, the files and the ceremony are the
users' gestures in the apps. Convention 14: every wait is a deadline poll on
state; the absence in (2) is read only after the successor's later file has
arrived, never after a settle.

tier_3: a real ``fauna-nest`` binary, two seats of the owner's app, and the
member's seat.
"""
from __future__ import annotations

import secrets

import pytest

from helpers.succession_ceremony import (
    SECRET_HEX_LEN,
    require_stolen_gate,
    settled_actor_id,
    wait_for_successor_actor,
)
from helpers.succession_file_door import seat_file_door, stolen_seat
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_folder_member_media_decrypt import _member_opens_and_reads
from tests.test_identity_succession_shared_folder_rekey import (
    _changes_as,
    _share_as_writer,
)

pytestmark = [
    pytest.mark.tier_3,
    # The cast — the member seat and (for the stolen writer) a second seat of
    # the app under test, each adding and reading files through its own door
    # (`helpers/succession_file_door.py`: a bound local folder on tui, linux
    # and macos, the Media page's upload and download on web and ios) — runs on
    # tui, linux, web, macos and ios. android's member seat is not built yet
    # (`surfaces.toml`'s `succession-witness-member-seat`); it joins with a
    # marker of its own.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.real_conversations,
]

#: A file crossing the nest between two bound folders.
_HYDRATION_S = 180.0
#: A nest round trip for a change-row read.
_ROSTER_S = 30.0


@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
def test_after_the_owner_takes_the_account_back_a_member_keeps_the_old_files_and_refuses_the_stolen_key(
    request, succession_member_app, succeedable_app, nest_instance, tmp_path
):
    """A holds a recovery kit, adds a file and shares the set to M → S (A's old
    seed) is bound and writes → A takes the account back → M still opens A's earlier file → S's
    later file is refused while the successor's later file arrives."""
    alice_app, alice = succeedable_app
    bob_app, bob = succession_member_app
    ab = alice_app.backups

    # ── 0. A holds a recovery kit before anyone joins (the module doc's ⚠) ──
    old_actor = settled_actor_id(alice_app)
    assert len(old_actor) == SECRET_HEX_LEN, (
        f"[A] the Status page must render the actor id; got {old_actor!r}, "
        f"error={alice_app.error_text()!r}"
    )
    alice_app.settings.navigate()
    alice_app.settings.open_recovery_kit()
    alice_app.settings.create_recovery_kit()
    alice_app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = alice_app.settings.recovery_kit_secret()
    assert len(held) == SECRET_HEX_LEN, f"the kit is 64-hex, got {len(held)}"

    # ── 1. A's bound set with a file of A's, shared to M, bound on M too ────
    set_name = f"heritage-{secrets.token_hex(4)}"
    ab.navigate_folders()
    ab.create_folder_via_wizard(set_name)
    a_door = seat_file_door(alice_app, set_name, tmp_path, seat="owner")
    _share_as_writer(alice_app, bob_app, bob, set_name)
    m_door = seat_file_door(
        bob_app, set_name, tmp_path, seat="member", shared_from=(nest_instance, alice)
    )
    request.addfinalizer(lambda: print(m_door.diagnosis()))
    request.addfinalizer(lambda: print(a_door.diagnosis()))
    file_a0 = f"owner-before-{secrets.token_hex(3)}.txt"
    payload_a0 = f"the owner's file, before the recovery — {secrets.token_hex(12)}\n"
    a_door.add(file_a0, payload_a0)
    wait_until(
        lambda: m_door.read(file_a0) == payload_a0,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[M] {file_a0}, A's file from before any ceremony, never reached M's "
            "copy — the share itself is broken, so nothing after the "
            f"recovery could be read as a result. error={bob_app.error_text()!r}\n"
            + m_door.diagnosis()
        ),
    )
    # Control for observable 1: M's Media page opens it before the ceremony.
    _member_opens_and_reads(bob_app, bob_app.driver, file_a0, payload_a0.encode())

    # ── 2. S: a seat of A's OLD seed, bound to the set, able to write ───────
    seat_gen = stolen_seat(
        alice_app, request, alice, nest_instance, screenshot_tag="succession-stolen-writer"
    )
    s_app, _s_driver = next(seat_gen)
    request.addfinalizer(lambda: seat_gen.close())
    s_door = seat_file_door(s_app, set_name, tmp_path, seat="stolen")
    file_s0 = f"stolen-before-{secrets.token_hex(3)}.txt"
    payload_s0 = f"written on the old seed while it was still the owner's — {secrets.token_hex(8)}\n"
    s_door.add(file_s0, payload_s0)
    wait_until(
        lambda: m_door.read(file_s0) == payload_s0,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[M] {file_s0}, added on the old seed's seat BEFORE the ceremony, never "
            "reached M — without it 'the stolen key's later write is refused' would "
            f"be a seat that could never write. error={bob_app.error_text()!r}\n"
            + s_door.diagnosis()
        ),
    )

    # ── 3. A takes the account back with the kit it held all along ─────────
    alice_app.settings.navigate()
    alice_app.settings.open_recovery_kit()
    require_stolen_gate(alice_app)
    alice_app.settings.succeed_identity_with_held_kit(held)
    new_actor = wait_for_successor_actor(alice_app, old_actor)
    assert new_actor != old_actor

    # ── 4. Observable 1: M still opens A's file from before ────────────────
    # A fresh Media re-entry re-judges the listing whole: A's row is signed by
    # the retired identity and served naming the successor.
    _member_opens_and_reads(bob_app, bob_app.driver, file_a0, payload_a0.encode())
    assert m_door.read(file_a0) == payload_a0, (
        f"[M] {file_a0} left M's copy after A's recovery — a member's copy of "
        "the owner's earlier file must stay"
    )

    # ── 5. Observable 2: the stolen key's later file is refused ────────────
    cursor = max(
        (c.get("seq", 0) for c in _changes_as(nest_instance, bob, set_name, 0)),
        default=0,
    )
    file_s1 = f"stolen-after-{secrets.token_hex(3)}.txt"
    payload_s1 = f"written on the stolen seed after the recovery — {secrets.token_hex(20)}\n"
    # The stolen seat's gesture is made and never awaited: the nest refuses the
    # retired key, so nothing of it may land.
    s_door.add(file_s1, payload_s1, await_landed=False)
    # The successor's later file is the clock the absence is read against.
    file_a1 = f"owner-after-{secrets.token_hex(3)}.txt"
    payload_a1 = f"the successor's file, after the recovery — {secrets.token_hex(12)}\n"
    a_door.add(file_a1, payload_a1)
    wait_until(
        lambda: m_door.read(file_a1) == payload_a1,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[M] {file_a1}, added by the successor after the recovery, never "
            f"reached M. error={bob_app.error_text()!r}\n" + m_door.diagnosis()
        ),
    )
    stolen_size = len(payload_s1.encode())
    rows = _changes_as(nest_instance, bob, set_name, cursor)
    stolen_rows = [
        c for c in rows
        if c.get("size_bytes") == stolen_size
        and c.get("author_actor_id") != bob["actor_id_hex"]
    ]
    assert not stolen_rows, (
        f"[nest] a change row of {stolen_size} bytes — the size of {file_s1}, "
        "written on the stolen seed after the recovery — landed on the set; the "
        f"retired key's write must be refused. rows={stolen_rows!r}"
    )
    assert m_door.read(file_s1) is None, (
        f"[M] {file_s1}, written on the stolen seed after the recovery, reached "
        "M — the stolen key's later write must be refused"
    )
    assert not bob_app.has_error(), (
        f"[M] the member's app surfaced an error: {bob_app.error_text()!r}"
    )


def _member_badges(app) -> list[str]:
    """Every ``folder-shared-badge`` text on M's Folders page, re-entered first so
    the rows repaint from the engine's current reading (the forked mark is read
    per listing, never pushed into an open page)."""
    mb = app.backups
    mb.navigate_devices()
    mb.navigate_folders()
    out: list[str] = []
    try:
        for i in range(app.driver.count("folder-shared-badge")):
            out.append(app.driver.get_text("folder-shared-badge", index=i))
    except LookupError:  # the list repainted under the read; next tick
        return []
    return out


def _owner_member_count(app, set_name: str) -> int | None:
    """How many ``folder-member-item`` rows the owner's expanded row lists for
    ``set_name`` — the page re-entered first, since the roster is read per
    expansion. ``None`` when the row could not be read this tick."""
    ob = app.backups
    try:
        ob.navigate_devices()
        ob.navigate_folders()
        ob.find_and_expand_folder(set_name)
        return ob.shared_member_count()
    except (LookupError, AssertionError):
        return None


def _owner_member_names(app, set_name: str) -> list[str] | str:
    """The handle (or actor id, when handle-less) of every ``folder-member-item``
    the owner's expanded row lists for ``set_name`` — what the count counts
    (convention 6), so a red names the identity still listed."""
    ob = app.backups
    try:
        ob.navigate_devices()
        ob.navigate_folders()
        row = ob.find_and_expand_folder(set_name)
        return [
            ob.member_handle(index=i, row=row) for i in range(ob.shared_member_count())
        ]
    except (LookupError, AssertionError) as e:
        return f"<unreadable: {e}>"


#: The badge's third reading, minus the owner's name, which the successor may
#: render differently from the identity M first knew.
_FORKED_SUFFIX = S.devices.shared_by_owner_changed(who="\x00").split("\x00", 1)[1]
_SHARED_BY_PREFIX = S.devices.shared_by(who="")


@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
def test_a_member_whose_copy_could_not_follow_the_owners_new_identity_is_re_shared_and_catches_up(
    request, succession_member_app, succeedable_app, nest_instance, tmp_path
):
    """A shares the set to M holding NO kit → M's harvest of A settles empty → A
    mints a kit and takes the account back → M's seat forks: M's badge reads the
    third text and the successor's member list stops naming M → the successor's
    ordinary share gesture re-shares → M's badge reads "Shared by" again and the
    successor's later file reaches M, while M's earlier files stay.

    ``federation-shared-folders.md`` § Cross-nest shared folders + channel
    append → *A forked seat is final, says so, and heals only by a fresh
    Welcome*: (a) the fork is detected on the folder walk and marked, (b) the
    seat drops its own roster row once, so the owner's ordinary re-share takes
    the ghost-heal arm (a fresh Welcome), (c) that Welcome replaces the forked
    seat with no relaunch.

    **The fork is the module doc's ⚠ run on purpose.** A member's peer-anchor
    harvest of the owner runs on a 5 s sweep soon after it joins and settles
    once per session; A holds no kit then, so it settles with no recovery head.
    The kit is minted only after two full file round trips across the set (A's
    file reaches M, M's file reaches A), so the harvest has long settled, and a
    kit minted after it is, to M, exactly what a thief's kit looks like: the
    statement is refused and the successor's remove-old forks M's seat.
    """
    alice_app, alice = succeedable_app
    bob_app, bob = succession_member_app
    ab = alice_app.backups

    old_actor = settled_actor_id(alice_app)
    assert len(old_actor) == SECRET_HEX_LEN, (
        f"[A] the Status page must render the actor id; got {old_actor!r}, "
        f"error={alice_app.error_text()!r}"
    )

    # ── 1. A's bound set with a file, shared to M (A holds NO kit) ─────────
    set_name = f"reshare-{secrets.token_hex(4)}"
    ab.navigate_folders()
    ab.create_folder_via_wizard(set_name)
    a_door = seat_file_door(alice_app, set_name, tmp_path, seat="owner")
    _share_as_writer(alice_app, bob_app, bob, set_name)
    m_door = seat_file_door(
        bob_app, set_name, tmp_path, seat="member", shared_from=(nest_instance, alice)
    )
    request.addfinalizer(lambda: print(m_door.diagnosis()))
    request.addfinalizer(lambda: print(a_door.diagnosis()))
    file_a0 = f"owner-before-{secrets.token_hex(3)}.txt"
    payload_a0 = f"the owner's file, before the recovery — {secrets.token_hex(12)}\n"
    a_door.add(file_a0, payload_a0)
    wait_until(
        lambda: m_door.read(file_a0) == payload_a0,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[M] {file_a0}, A's file from before any ceremony, never reached M's "
            f"copy — the share itself is broken. error={bob_app.error_text()!r}\n"
            + m_door.diagnosis()
        ),
    )

    # ── 2. M's own file reaches A: the seat writes, and it stays M's ───────
    file_m0 = f"member-before-{secrets.token_hex(3)}.txt"
    payload_m0 = f"the member's file, before the recovery — {secrets.token_hex(12)}\n"
    m_door.add(file_m0, payload_m0)
    wait_until(
        lambda: a_door.read(file_m0) == payload_m0,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[A] {file_m0}, M's file from before the ceremony, never reached A. "
            f"error={alice_app.error_text()!r}\n" + a_door.diagnosis()
        ),
    )
    healthy = [b for b in _member_badges(bob_app) if _FORKED_SUFFIX in b]
    assert not healthy, (
        f"[M] a seat that has seen no ceremony must not read forked: {healthy!r}"
    )

    # ── 3. A mints a kit only now, and takes the account back with it ──────
    alice_app.settings.navigate()
    alice_app.settings.open_recovery_kit()
    alice_app.settings.create_recovery_kit()
    alice_app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = alice_app.settings.recovery_kit_secret()
    assert len(held) == SECRET_HEX_LEN, f"the kit is 64-hex, got {len(held)}"
    alice_app.settings.navigate()
    alice_app.settings.open_recovery_kit()
    require_stolen_gate(alice_app)
    alice_app.settings.succeed_identity_with_held_kit(held)
    new_actor = wait_for_successor_actor(alice_app, old_actor)
    assert new_actor != old_actor

    # ── 4. M's seat forks: the badge's third reading, and M leaves the list ─
    wait_until(
        lambda: any(_FORKED_SUFFIX in b for b in _member_badges(bob_app)),
        _HYDRATION_S,
        interval=2.0,
        diagnose=lambda: (
            f"[M] the badge never read the third text ({_FORKED_SUFFIX!r}) after "
            f"the owner's recovery; badges={_member_badges(bob_app)!r} "
            f"error={bob_app.error_text()!r}\n"
            # Whether M refused the statement (the fork's premise) or accepted
            # it (no fork to paint): the witness's own report says which.
            f"[M] data.succession_witness="
            f"{bob_app.driver.get_state('data.succession_witness')!r}\n"
            + m_door.diagnosis()
        ),
    )
    wait_until(
        lambda: _owner_member_count(alice_app, set_name) == 0,
        _HYDRATION_S,
        interval=2.0,
        diagnose=lambda: (
            "[A] the successor's 'Shared with' list still names M after M's seat "
            f"forked; members={_owner_member_names(alice_app, set_name)!r} "
            f"(M={bob.get('handle')!r}/{bob.get('actor_id_hex')!r}, the retired "
            f"owner id={old_actor!r}, the successor={new_actor!r}) "
            f"error={alice_app.error_text()!r}"
        ),
    )

    # ── 5. The successor's ordinary share gesture; M's row heals ───────────
    _share_as_writer(alice_app, bob_app, bob, set_name)
    mb = bob_app.backups

    def _healed() -> bool:
        badges = _member_badges(bob_app)
        if any(b.startswith(_SHARED_BY_PREFIX) and _FORKED_SUFFIX not in b for b in badges):
            return True
        try:  # a stranger-grade arrival raises a knock; M accepts it
            if mb.pending_share_count() >= 1:
                mb.accept_pending_share(0)
        except Exception:  # noqa: BLE001 — the knock row repaints; next tick
            pass
        return False

    wait_until(
        _healed,
        _HYDRATION_S,
        interval=2.0,
        diagnose=lambda: (
            "[M] after the successor's re-share M's row never read 'Shared by' "
            f"without the third text; badges={_member_badges(bob_app)!r} "
            f"error={bob_app.error_text()!r}\n" + m_door.diagnosis()
        ),
    )

    # ── 6. The successor's later file reaches M; M's earlier files stay ────
    file_a1 = f"owner-after-{secrets.token_hex(3)}.txt"
    payload_a1 = f"the successor's file, after the re-share — {secrets.token_hex(12)}\n"
    a_door.add(file_a1, payload_a1)
    wait_until(
        lambda: m_door.read(file_a1) == payload_a1,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[M] {file_a1}, added by the successor after the re-share, never "
            f"reached M. error={bob_app.error_text()!r}\n" + m_door.diagnosis()
        ),
    )
    for name, payload in ((file_a0, payload_a0), (file_m0, payload_m0)):
        assert m_door.read(name) == payload, (
            f"[M] {name}, in M's copy before the recovery, did not stay through "
            "the fork and the re-share"
        )
    assert not bob_app.has_error(), (
        f"[M] the member's app surfaced an error: {bob_app.error_text()!r}"
    )
