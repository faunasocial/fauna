"""tier_3 e2e: a file added to an inherited shared folder after the recovery
cannot be opened with the stolen key, while the folder's other members read it.

Outcome 35 of ``docs/features/take-your-account-back.md``;
``docs/goal/behavior/succession-aftermath.md`` § Re-key scope, the MLS groups
row — *a bound folder set's content key rotates behind remove-old* (ruled and
built 2026-10-03; § Implementation status today). Until that build the
ceremony's sweep removed the retired seat from the set's group but minted no
generation, so the custody a stolen seed held still opened every file added to
an inherited set afterwards. The rotation runs in the successor's folders
launch pass (``FoldersAuthor::rotate_succeeded_sets``) once remove-old has left
the group; it is pinned over the real chain at tier_3 in Rust
(``conformance_shared_folders::a_succession_rotates_the_inherited_sets_content_key_over_the_real_sweep_real_nest``).
What no Rust pin reaches is three real apps against one real nest.

**The three parties.**

- **A** — the app under test, a dedicated actor (``succeedable_app``): creates
  a set, binds a folder, shares it to M as a writer, then takes the account
  back and becomes the successor.
- **M** — a second actor's real-engine seat (``succession_member_app``), a
  writer member of A's set with a bound folder of its own.
- **S** — a second seat of A's OLD seed (a second launch of A's app; a twin
  page on web), launched before the ceremony and left running through it: the
  thief's device. The ceremony carries no device
  across (``succession-propagation.md`` § Propagation → *Own device fleet*: a
  device keeps the old seed until its user imports the new one), so S is
  exactly what a seed thief holds.

**The observable, and why it is the honest one.** "Cannot be opened" is a
mechanism, not an appearance, so it is read off the mechanism: a file sealed under a content-key generation opens for a
holder exactly when that holder's custody holds that generation's key. The test
reads three things, none of them a UI paint:

1. **the generation the file was sealed under** — the ``content_key_version``
   on the nest's change row for M's upload, found by its author and its size
   (``fauna.sync.changes.list`` as M, a member's ordinary read);
2. **which key that generation is** — the successor's own custody
   (``folder_set_custody``, a one-way fingerprint per generation, never the
   key);
3. **what the stolen seed holds** — S's custody, read before the ceremony (the
   custody a thief walks away with) and again after it (anything the running
   thief's device picked up since).

The file is unopenable with the stolen key iff no fingerprint S ever held is
the fingerprint of the generation the file was sealed under. The other half —
"the folder's other members still read it" — is A producing the file's bytes
intact: materializing in A's bound folder (A's engine opened it), or, on web,
A's Media-page download of it (no key, no plaintext). Each seat adds and reads
through its own door (``helpers/succession_file_door.py``); the assertions are
the same on every app.

⚠ **Each read has its control, so no green is vacuous.** S's pre-ceremony
custody must hold the very key the set had then (the successor's own
generation-1 print), or "S lacks the new key" would be a seat that never held
the set at all. The successor's custody must show a NEW generation past the
one S holds, or there is no rotation to lack. And the change row must name a
generation, or the comparison has nothing to compare.

⚠ **The nest's refusal of S's sign-in is NOT this observable.** After the
ceremony the nest refuses the retired key (outcome 27), so S fetches nothing
— a door, not the seal. This test asserts the seal: even the custody S holds
locally opens nothing M added after the rotation.

Convention 8: the set, the share, the promotion, the binds, the files and the
ceremony are the users' gestures in the apps. Convention 14: every wait is a
deadline poll on state, never a settle; a green run pays none of its budget.

tier_3: a real ``fauna-nest`` binary, two seats of the app under test, and the
member's seat.
"""
from __future__ import annotations

import secrets

import pytest

from helpers.diagnostics import aftermath_log_lines as _aftermath_log
from helpers.set_names import addressed
from helpers.succession_ceremony import (
    SECRET_HEX_LEN,
    require_stolen_gate,
    settled_actor_id,
    wait_for_successor_actor,
)
from helpers.succession_file_door import seat_file_door, stolen_seat
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # Every app here carries the custody reader (`folder_set_custody`, the
    # shared `fauna_client_folders::custody::read_held_generations`) and the
    # member seat; each seat adds and reads files through its own door
    # (`helpers/succession_file_door.py`): a bound local folder on tui, linux
    # and macos, the Media page's upload and download on web, which keeps no
    # bound folder, and on ios, which runs no sync agent of its own. android's
    # member seat is not built yet (`surfaces.toml`'s
    # `succession-witness-member-seat`); it joins with a marker of its own.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.real_conversations,
]

#: A share landing on the owner's roster, a promotion persisting, a knock
#: reaching the member: one nest round trip each, sized like the share journeys'.
_ROSTER_S = 30.0
#: A file crossing the nest between two bound folders (the share journeys'
#: `_NEST_HYDRATION_S`).
_HYDRATION_S = 180.0
#: The successor's launch pass after the switch: the custody reconcile, then the
#: rotation and its envelope publish. Sized like the aftermath journeys.
_ROTATION_S = 180.0
#: A seat's account runtime replicating the custody a sibling wrote.
_CUSTODY_SYNC_S = 120.0


def _prints(reading: dict) -> dict[str, int]:
    """``{key_fp: version}`` for every generation a custody reading holds."""
    return {g["key_fp"]: g["version"] for g in reading.get("generations", [])}


def _current(reading: dict) -> dict | None:
    gens = reading.get("generations") or []
    return gens[0] if gens else None


def _changes_as(nest, actor, set_name: str, since: int) -> list[dict]:
    """The set's change rows past ``since``, read as ``actor`` (a member's
    ordinary read: the owner-or-member gate)."""
    from common.auth import _user_call

    reply = _user_call(
        nest["port"],
        actor["signing_key"].encode().hex(),
        "fauna.sync.changes.list",
        addressed(set_name, since=since),
        nest["url"],
    )
    return list(reply.get("changes") or [])


def _share_as_writer(alice_app, bob_app, bob, set_name: str) -> None:
    """A shares ``set_name`` to M and promotes M to writer (a reader never binds
    an engine); M takes the share — accepting a knock if one is raised, the
    contact edge the member fixture arranges may land it straight away."""
    ob = alice_app.backups
    row = ob.find_and_expand_folder(set_name)
    if not ob.share_button_visible():
        ob.expand_folder(row)
    ob.open_share_dialog()
    ob.share_recipient(handle=bob["handle"], actor_id_hex=bob["actor_id_hex"])
    wait_until(
        lambda: ob.shared_member_count() == 1,
        _ROSTER_S,
        diagnose=lambda: (
            f"[A] the share should land exactly one member; roster reads "
            f"{ob.shared_member_count()}, error={alice_app.error_text()!r}"
        ),
    )
    ob.set_member_access("writer", 0, row=row)

    def _writer() -> bool:
        try:
            return ob.member_access(0, row=row) == "writer"
        except LookupError:
            return False

    wait_until(
        _writer,
        _ROSTER_S,
        diagnose=lambda: (
            "[A] M must persist as writer — a reader never binds an engine, so "
            f"M could add no file; error={alice_app.error_text()!r}"
        ),
    )

    mb = bob_app.backups
    mb.navigate_folders()

    def _taken() -> bool:
        if any(set_name in t for t in mb.folder_titles()):
            return True
        try:
            if mb.pending_share_count() >= 1:
                mb.accept_pending_share(0)
        except Exception:  # noqa: BLE001 — the knock row repaints; next tick
            pass
        mb.navigate_devices()
        mb.navigate_folders()
        return False

    wait_until(
        _taken,
        90.0,
        interval=1.0,
        diagnose=lambda: (
            f"[M] the shared set {set_name!r} never appeared on M's Folders page; "
            f"titles={mb.folder_titles()!r} error={bob_app.error_text()!r}"
        ),
    )


@pytest.mark.timeout(1800)
@pytest.mark.feature("take-your-account-back")
def test_a_file_added_after_the_recovery_does_not_open_under_the_stolen_key(
    request, succession_member_app, succeedable_app, nest_instance, tmp_path
):
    """share a bound set A→M → S (A's old seed) holds its key → A takes the
    account back → the set's key rotates → M adds a file → A reads it, and no
    key S ever held is the one it was sealed under (outcome 35)."""
    alice_app, alice = succeedable_app
    bob_app, bob = succession_member_app
    ab, mb = alice_app.backups, bob_app.backups

    # ── 0. A holds a recovery kit before anyone joins ───────────────────────
    # The order is the honest one: M verifies the ceremony's in-group statement
    # only against a recovery head its once-per-session harvest of the set's
    # owner read BEFORE the ceremony, and an owner with no kit publishes none.
    # A kit minted after M's harvest and spent seconds later is, to M, exactly a
    # seed thief's, so M refuses it and forks off the set's group
    # (`test_folder_succession_member_reader.py`'s module doc has the whole
    # chain).
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

    # ── 1. A's bound set, shared to M as a writer, bound on both sides ──────
    set_name = f"inherited-{secrets.token_hex(4)}"
    ab.navigate_folders()
    ab.create_folder_via_wizard(set_name)
    a_door = seat_file_door(alice_app, set_name, tmp_path, seat="owner")
    _share_as_writer(alice_app, bob_app, bob, set_name)
    m_door = seat_file_door(
        bob_app, set_name, tmp_path, seat="member", shared_from=(nest_instance, alice)
    )
    request.addfinalizer(lambda: print(m_door.diagnosis()))
    request.addfinalizer(lambda: print(a_door.diagnosis()))

    # The ground the rest stands on: a file M adds BEFORE the ceremony reaches A.
    file_0 = f"before-{secrets.token_hex(3)}.txt"
    payload_0 = f"before the recovery — {secrets.token_hex(8)}\n"
    m_door.add(file_0, payload_0)
    wait_until(
        lambda: a_door.read(file_0) == payload_0,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[A] {file_0}, added by M before any ceremony, never reached A — the "
            "share itself is broken, so nothing after the recovery could be read "
            f"as a result. error={alice_app.error_text()!r}\n" + a_door.diagnosis()
        ),
    )

    a_before = ab.folder_set_custody(set_name=set_name)
    assert a_before.get("found") and _current(a_before), (
        f"[A] the owner's custody must hold the bound set's key: {a_before!r}"
    )
    channel_hex = a_before["channel_id_hex"]
    gen_before = _current(a_before)

    # ── 2. S: a seat of A's OLD seed, holding the set's key before the ceremony ──
    # S runs the app under test: the thief's device runs the same app the
    # owner does.
    seat_gen = stolen_seat(
        alice_app, request, alice, nest_instance, screenshot_tag="succession-stolen-seat"
    )
    s_app, _s_driver = next(seat_gen)
    request.addfinalizer(lambda: seat_gen.close())
    s_readings: list[dict] = []

    def _s_holds_the_set() -> bool:
        reading = s_app.backups.folder_set_custody(set_name=set_name)
        s_readings.append(reading)
        return gen_before["key_fp"] in _prints(reading)

    wait_until(
        _s_holds_the_set,
        _CUSTODY_SYNC_S,
        interval=2.0,
        diagnose=lambda: (
            "[S] the old seed's seat must hold the set's key BEFORE the ceremony, "
            "or 'it holds no key for the new generation' would be a seat that never "
            f"held the set at all. A's current generation {gen_before!r}; S's last "
            f"reading {s_readings[-1] if s_readings else None!r}; "
            f"error={s_app.error_text()!r}"
        ),
    )
    stolen = dict(_prints(s_readings[-1]))

    # ── 3. A takes the account back with the kit it held all along ─────────
    alice_app.settings.navigate()
    alice_app.settings.open_recovery_kit()
    require_stolen_gate(alice_app)
    alice_app.settings.succeed_identity_with_held_kit(held)
    new_actor = wait_for_successor_actor(alice_app, old_actor)
    assert new_actor != old_actor

    # ── 4. The successor's launch pass rotates the inherited set's key ──────
    a_readings: list[dict] = []

    def _rotated() -> bool:
        reading = ab.folder_set_custody(set_name=set_name)
        a_readings.append(reading)
        cur = _current(reading)
        return bool(cur) and cur["version"] > gen_before["version"]

    wait_until(
        _rotated,
        _ROTATION_S,
        interval=2.0,
        diagnose=lambda: (
            "[A] the successor's custody never showed a generation past the one the "
            f"stolen seed holds ({gen_before!r}) — the succession did not rotate the "
            "inherited set's content key (succession-aftermath.md § Re-key scope, "
            "the MLS groups row). Last reading "
            f"{a_readings[-1] if a_readings else None!r}; aftermath log:"
            f"{_aftermath_log(alice_app)}; error={alice_app.error_text()!r}"
        ),
    )
    rotated = _current(a_readings[-1])
    assert rotated["key_fp"] not in stolen, (
        f"[A] the rotated generation's key is one the stolen seed already holds: "
        f"rotated={rotated!r} stolen={stolen!r}"
    )
    # The rotated generation itself must be reachable by the member before M
    # writes — otherwise M's engine could only seal under what it already held.
    m_readings: list[dict] = []

    def _m_holds_rotation() -> bool:
        reading = mb.folder_set_custody(channel_id_hex=channel_hex)
        m_readings.append(reading)
        return rotated["key_fp"] in _prints(reading)

    wait_until(
        _m_holds_rotation,
        _ROTATION_S,
        interval=2.0,
        diagnose=lambda: (
            "[M] the member never ingested the rotated generation "
            f"{rotated!r}; last reading {m_readings[-1] if m_readings else None!r}; "
            f"error={bob_app.error_text()!r}\n" + m_door.diagnosis()
        ),
    )

    # ── 5. M adds a file after the rotation; A, a member, reads it ──────────
    cursor = max((c.get("seq", 0) for c in _changes_as(nest_instance, bob, set_name, 0)), default=0)
    file_1 = f"after-{secrets.token_hex(3)}.txt"
    payload_1 = f"after the recovery, for members only — {secrets.token_hex(24)}\n"
    m_door.add(file_1, payload_1)
    wait_until(
        lambda: a_door.read(file_1) == payload_1,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[A] {file_1}, added by M after the recovery, never reached the "
            "successor — the folder's other members must still read it. "
            f"error={alice_app.error_text()!r}\n" + a_door.diagnosis()
        ),
    )

    size = len(payload_1.encode())
    rows: list[dict] = []

    def _m_row() -> bool:
        rows[:] = [
            c
            for c in _changes_as(nest_instance, bob, set_name, cursor)
            if c.get("author_actor_id") == bob["actor_id_hex"] and c.get("size_bytes") == size
        ]
        return bool(rows)

    wait_until(
        _m_row,
        _ROSTER_S,
        diagnose=lambda: (
            f"[nest] no change row past seq {cursor} by M ({bob['actor_id_hex'][:12]}…) "
            f"of {size} bytes for {file_1}; rows past the cursor: "
            f"{_changes_as(nest_instance, bob, set_name, cursor)!r}"
        ),
    )
    sealed_under = rows[-1].get("content_key_version")
    assert sealed_under is not None, (
        f"[nest] M's change row names no content-key generation: {rows[-1]!r}"
    )
    a_after = ab.folder_set_custody(set_name=set_name)
    file_prints = [fp for fp, v in _prints(a_after).items() if v == sealed_under]
    assert file_prints, (
        f"[A] the successor's custody holds no key for generation {sealed_under}, "
        f"the one M's file was sealed under, although A read the file: {a_after!r}"
    )

    # ── 6. No key the stolen seed ever held opens it ────────────────────────
    s_after = s_app.backups.folder_set_custody(set_name=set_name)
    if s_after.get("found"):
        stolen.update(_prints(s_after))
    opened_by_stolen = [fp for fp in file_prints if fp in stolen]
    assert not opened_by_stolen, (
        f"[S] the file M added after the recovery was sealed under generation "
        f"{sealed_under}, whose key the stolen seed's seat holds — the stolen key "
        f"opens it (succession-aftermath.md § Re-key scope). stolen={stolen!r} "
        f"file generation prints={file_prints!r} rotated={rotated!r} "
        f"S after the ceremony={s_after!r}"
    )
    assert sealed_under > gen_before["version"], (
        f"[nest] M's file was sealed under generation {sealed_under}, not past the "
        f"pre-ceremony generation {gen_before['version']} — M did not seal under the "
        f"rotated key. row={rows[-1]!r}"
    )
