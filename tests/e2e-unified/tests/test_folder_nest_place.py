"""tier_3 — the nest place's snapshot policy, driven from the folder row
(`folder-nest-*`; folders re-model phase 2 slice e, element IDs user-approved
2026-08-15).

Slice d landed the policy at rest and on the wire (schema v37,
`folders.{nest_snapshots,nest_snapshot_quiet_secs}`, `nest_place` on
`fauna.folders.update` and both `FolderSummary` projections) with **no app able
to reach it** — the one-configuration-surface invariant says a capability absent
from the app UI is not configurable, so the columns rested unset on every folder.
This is the test that the editor closes that gap.

MUTATION is UI-driven throughout (convention 8 — the app is the only config
surface, so the knobs are typed and clicked, never written by a raw RPC).
VERIFICATION reads the nest's ground truth through `fauna.folders.list`, the
external black-box check the sibling `test_folder_webdav_toggle.py` uses.

**Three-state is the whole point, and it is what this test guards.** Each knob is
on / off / *unset*, where unset means "nothing authoritative said" — the resting
value of every folder, and where a knob RETURNS when the user picks the default.
A `NOT NULL DEFAULT` was deliberately refused nest-side (`backup-restore.md`
§ 8b), so an editor that could not express "unset" would strand every folder it
touched on a value its owner never chose. The clear-back-to-unset leg below is
therefore not an edge case — it is the reason the select has three options
instead of being a checkbox.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.set_names import find_set

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui is the lead app (rust-first ordering): it carries the editor first and
    # the other six follow in their batched trickle-down. Each
    # app joins this list as its leg lands — the marker list IS the parity
    # ledger, so a missing app here is visible lag, not a silent gap.
    pytest.mark.tui,
    pytest.mark.linux,  # 2026-08-16, the first trickle-down leg
    pytest.mark.web,  # 2026-08-18, the version-retention pair — run green here
    pytest.mark.macos,  # 2026-08-18, the apple leg — run green on macOS
    pytest.mark.windows,  # 2026-09-09, first green run of the code built blind 2026-08-21
    # 2026-09-21: the iOS leg's HARNESS cost gate is discharged — it was never a
    # leg gap (BUILT 2026-08-18 by the SAME commit as macOS: one shared
    # `FoldersContent.FolderNestPlaceEditor` renderer feeds both targets, called
    # unconditionally, with no `#if os(...)` between them), only the full
    # 5-slice `just apple-ffi` xcframework an `--app ios` run needs. A macOS host
    # trickle-down pass carries that build anyway, so the run is now cheap here.
    pytest.mark.ios,
    # NOT here yet, and the absence names its reason rather than reading as an
    # unbuilt app — the ledger only means "visible lag" if the entries are honest:
    #   * android — BUILT 2026-08-16, proven by Robolectric
    #     (`FoldersContentTest`); android has no e2e path on the Linux dev machine
    #     at all, so this marker is gated on the host emulator, not on the leg.
]


def _user_client(nest_instance, test_user):
    """A User-class WS-RPC client on the logged-in actor — the ground-truth read
    for the folder's `nest_place` policy."""
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _binds_nothing(retention: str | None) -> bool:
    """Whether a `retention_policy` column value keeps everything.

    Asserts MEANING, not storage spelling: absent, empty, and the canonical
    `{"max_snapshots":0,"max_age_days":0}` are all `FolderRetention::NotSet` to
    the nest by design (`backup/retention.rs::parse_folder_retention` — "a zero
    in either field means that bound is unset ... a policy that binds nothing is
    not a policy"). The client CLEARS by sending that canonical value rather
    than `None`, because the wire field's `None` means "leave unchanged" — so a
    test that demanded `None` here would be testing the spelling and failing the
    user's actual question, "did my retention go away?".
    """
    if retention is None or not retention.strip():
        return True
    import json

    try:
        p = json.loads(retention)
    except ValueError:
        return False
    return int(p.get("max_snapshots", 0)) == 0 and int(p.get("max_age_days", 0)) == 0


def _version_binds_nothing(vr) -> bool:
    """Whether a `version_retention` projection value keeps everything.

    Same meaning-not-spelling discipline as `_binds_nothing`: the nest rests a
    cleared policy as `NULL` (projection omits the field), but a both-zero pair
    is the same keep-everything statement (`file-versions.md` § Retention
    ruling 1 — "a 0 in a bound means that bound is unset")."""
    if vr in (None, {}):
        return True
    return (
        int(vr.get("max_versions_per_path", 0)) == 0
        and int(vr.get("max_age_days", 0)) == 0
    )


def _row(client, name: str) -> dict:
    """The nest's own row for `name`, or a clear failure naming what it saw."""
    reply = client.call("fauna.folders.list", {})
    row = find_set(reply.get("folders", []), name)
    if row is not None:
        return row
    raise AssertionError(
        f"nest has no folder {name!r}; it listed "
        f"{[f.get('name') for f in reply.get('folders', [])]}"
    )


@pytest.mark.feature("folders")
def test_nest_place_policy_is_reachable_editable_and_clearable(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    b = app.backups
    name = f"nest-place-{secrets.token_hex(4)}"

    b.navigate_folders()
    b.create_folder_via_wizard(name)

    gt = _user_client(nest_instance, test_user)
    with gt:
        # 1. A fresh folder RESTS unset — not `false`, not `0`. If a default ever
        #    leaks in nest-side, this is where it surfaces, before any UI acts.
        row = _row(gt, name)
        assert row.get("nest_place") in (None, {}), (
            "a fresh folder must rest with no nest-place policy — an unset knob "
            f"is the honest resting value (backup-restore.md § 8b); got {row.get('nest_place')!r}"
        )
        assert row.get("retention_policy") in (None, ""), (
            f"a fresh folder must keep everything; got {row.get('retention_policy')!r}"
        )
        assert _version_binds_nothing(row.get("version_retention")), (
            "a fresh folder must keep every file version — `NULL` is the honest "
            "resting value (file-versions.md § Retention ruling 1); got "
            f"{row.get('version_retention')!r}"
        )

        # 2. The editor renders on an ORDINARY folder — not a backup-type one,
        #    which is the point of moving retention off the wizard: what the nest
        #    keeps is a property of the one place every folder has.
        b.find_and_expand_folder_until(name, "folder-nest-snapshots-select")

        # 3. Set all six knobs through the UI and save them together — the
        #    snapshot policy and its version-retention SIBLING (file-versions.md
        #    § Retention ruling 1) ride the same `fauna.folders.update`, each
        #    family sent whole.
        b.set_nest_snapshots("on")
        b.set_nest_quiet("120")
        b.set_nest_retention(snapshots="5", days="10")
        b.set_version_retention(count="3", days="30")
        b.save_nest_place()

        row = _wait_row(gt, name, lambda r: (r.get("nest_place") or {}).get("snapshots") is True)
        place = row.get("nest_place") or {}
        assert place.get("snapshots") is True, f"snapshots knob did not land: {place!r}"
        assert place.get("quiet_secs") == 120, f"quiet period did not land: {place!r}"
        retention = row.get("retention_policy") or ""
        assert '"max_snapshots":5' in retention.replace(" ", ""), retention
        assert '"max_age_days":10' in retention.replace(" ", ""), retention
        vr = row.get("version_retention") or {}
        assert vr.get("max_versions_per_path") == 3, (
            f"the version-count bound did not land: {vr!r}"
        )
        assert vr.get("max_age_days") == 30, (
            f"the version-age bound did not land: {vr!r}"
        )

        # 4. OFF is a real, distinct third value — not the same as unset. A
        #    folder whose owner said "don't keep snapshots" must read back
        #    `false`, or the owner's explicit no is indistinguishable from never
        #    having chosen.
        b.navigate_folders()
        b.find_and_expand_folder_until(name, "folder-nest-snapshots-select")
        b.set_nest_snapshots("off")
        b.save_nest_place()
        row = _wait_row(gt, name, lambda r: (r.get("nest_place") or {}).get("snapshots") is False)
        assert (row.get("nest_place") or {}).get("snapshots") is False, (
            "an explicit 'don't keep snapshots' must persist as false, distinct "
            f"from unset; got {row.get('nest_place')!r}"
        )

        # 5. Back to the default, and empty the boxes: the policy CLEARS. This is
        #    the leg that proves the third state is reachable in both directions
        #    — without it a user could set a knob but never take it back, and
        #    every folder they touched would rest on a value forever.
        b.navigate_folders()
        b.find_and_expand_folder_until(name, "folder-nest-snapshots-select")
        b.set_nest_snapshots("default")
        b.set_nest_quiet("")
        b.set_nest_retention(snapshots="", days="")
        b.set_version_retention(count="", days="")
        b.save_nest_place()

        row = _wait_row(
            gt, name, lambda r: (r.get("nest_place") or {}).get("snapshots") is None
        )
        place = row.get("nest_place") or {}
        assert place.get("snapshots") is None, f"snapshots did not clear: {place!r}"
        assert place.get("quiet_secs") is None, f"quiet period did not clear: {place!r}"
        assert _binds_nothing(row.get("retention_policy")), (
            "emptying both retention boxes must leave a policy that BINDS NOTHING "
            "— keep-everything, never 'keep zero snapshots'; got "
            f"{row.get('retention_policy')!r}"
        )
        assert _version_binds_nothing(row.get("version_retention")), (
            "emptying both version boxes must leave version history unbounded "
            "again — keep-everything, never 'keep zero versions'; got "
            f"{row.get('version_retention')!r}"
        )


def _wait_row(client, name: str, predicate, timeout: float = 15.0) -> dict:
    """Poll the nest row until `predicate` holds, then return it.

    A deadline poll on latency-independent STATE, never a settle-sleep
    (convention 14): a green run pays only the real round-trip, and the budget is
    sized far above any non-pathological delay on a loaded box.
    """
    import time

    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = _row(client, name)
        if predicate(last):
            return last
        time.sleep(0.3)
    raise AssertionError(
        f"folder {name!r} never reached the expected nest-place state; "
        f"last row: nest_place={last.get('nest_place')!r} "
        f"retention_policy={last.get('retention_policy')!r}"
    )
