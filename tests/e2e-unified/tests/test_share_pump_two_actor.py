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
(``e2e_login_device_id`` vs ``_E2E_SHARE_RECIPIENT_DEVICE_ID``) — the
lesson: same-id seats classify each other's rows as their own echo and the
file silently never arrives.
"""

import re
import secrets

import pytest

from common.nest import start_nest_in_place, stop_nest
from conftest import get_available_apps
from helpers.waiting import (
    await_devices_refresh_after,
    devices_refresh_baseline,
    share_ingest_reading,
    wait_until,
)
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
from helpers.macos_firewall import seat_firewall_note
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # tui leads the plane (`p2p.md` § Cross-user shared-set transfer → build
    # order); linux joined 2026-08-21. Each further
    # leg arrives with its own marker AND an `_SUPPORTED_APPS` entry — a
    # marker alone silently keeps driving tui underneath (the addendum
    # item 9, the same trap on the ceremony's own journey).
    pytest.mark.tui,
    pytest.mark.linux,
    # macOS joined 2026-09-25 as the MEMBER seat of a tui owner, and on
    # 2026-10-24 as an OWNER seat too (`_OWNER_APPS`, `_MIXED_PAIRS`):
    # `--app macos` drives macOS × macOS, `--app macos,tui` adds both mixed arms.
    pytest.mark.macos,
    # windows joined 2026-10-10 as an OWNER and a MEMBER seat (it has
    # `folder-member-role-select`, and its TestAgent carries the three serve /
    # probe doors); its real external `fauna-sync-agent` is what the plane's
    # provisioner drives.
    pytest.mark.windows,
    # iOS joined 2026-11-13 as the MEMBER seat of a tui owner (`p2p-shared-set-build.md`
    # § Phone peers — design, decision 5: a phone dials out more easily than it is
    # dialled, so it is first proven as the member). Its seat is the Debug
    # `Fauna-iOS` app with the File Provider appex (`ios_file_provider_app`, on the
    # param only — `_share_plane_seat_params`), whose extension is the replica the
    # app's foreground plane lands into (decision 3).
    pytest.mark.ios,
    # macOS' production spawner is launchd, which an e2e launch must not
    # bootstrap; this marker is what makes its launch construct the private
    # child-agent spawner instead, and the plane's provisioner (its two agent
    # verbs) is built from that spawner — no agent, no provisioner, no plane.
    # A no-op for tui/linux, which direct-spawn their agent already.
    pytest.mark.real_sync_agent,
    # windows spawns its agent DETACHED: this puts the owner's agent on the
    # run's own pipe/data dir, gives each fresh windows seat one of its own,
    # and stitches that agent's log into `app_stderr_text` (which
    # `await_agent_upload` greps). A no-op on the other apps
    # (`test_webdav_shared_set.py` precedent).
    pytest.mark.isolated_sync_agent,
]

#: Apps with a landed share-plane leg. Grow this — and the app's own marker
#: above — together.
_SUPPORTED_APPS = ("tui", "linux", "macos", "windows", "ios")

#: The apps that can be the OWNER seat of this journey: the choreography has
#: the owner promote the member to **writer** (readers never bind engines),
#: which needs `folder-member-role-select`. Every supported app now carries it
#: (macOS since the shared FaunaKit `FolderSharingViews` grant surface), so
#: each app's leg is proven as the SERVING seat as well as the member one.
_OWNER_APPS = ("tui", "linux", "macos", "windows")

#: The owner a supported app that cannot own drives its member seat against —
#: the lead app, which is also the one macOS can host next to macOS.
_LEAD_OWNER_APP = "tui"

#: Mixed (owner, member) pairs run beside the same-app ones, both directions:
#: macOS's member seat was first proven under a tui owner and keeps that arm,
#: and its serving half is witnessed against the lead app's member too — the
#: cross-app pairing a defect in shared glue would otherwise hide behind a
#: same-app arm whose two sides break alike.
_MIXED_PAIRS = (("tui", "macos"), ("macos", "tui"))


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
    pairs.extend(
        (owner, member)
        for owner, member in _MIXED_PAIRS
        if owner in available and member in available
    )
    return pairs


def _share_plane_seat_params():
    """`_share_plane_seat_pairs` as pytest params: an iOS member seat also carries
    `ios_file_provider_app`, so the harness installs the appex-embedding app and
    lifts the File Provider reconcile gate for that seat alone (`conftest.py`'s
    `_ios_seat_app_path`)."""
    return [
        pytest.param(owner, member, marks=[pytest.mark.ios_file_provider_app])
        if member == "ios"
        else (owner, member)
        for owner, member in _share_plane_seat_pairs()
    ]

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
# One Folders refresh committing (each read in it carries its own deadline);
# the outer post-restart poll re-visits until one reaches the restarted nest.
_FOLDERS_REFRESH_S = 60.0
# Leaving Folders for the feed and coming back is the nav edge that refreshes
# the page on every app (linux spawns it off the page's map hook).
_FEED_NAV = {"nav": {"stack": [{"view": "feed"}]}}


def _read_or_none(path):
    try:
        return path.read_text()
    except (FileNotFoundError, OSError):
        return None


def _is_ios(app) -> bool:
    from helpers.app_surface import app_name

    return app_name(app.driver) == "ios"


def _ios_staging_root(udid: str, member_hex: str, folder_id: int):
    """The set's staging root on the member's simulator — the extension's kept
    root, where a peer-landed body rests (`p2p-shared-set-build.md` § Built —
    iOS's leg; `FileProviderDomainIdentity.stagingRootRelativePath`:
    `FileProvider/roots/<actor-id-hex>/<ref-component>`), read from the host the
    way the eviction witness reads the replica, so no launch ends the seat."""
    from pathlib import Path

    from tests.platform.ios.test_file_provider_eviction import _BUNDLE_ID, _sim

    listed = _sim("get_app_container", udid, _BUNDLE_ID, "groups").stdout
    groups = [Path(e.split(None, 1)[-1].strip()) for e in listed.splitlines() if e.strip()]
    base = next((g for g in groups if (g / "FileProvider").is_dir()), None)
    assert base is not None, (
        f"[member] no app group on {udid} holds a FileProvider directory after the "
        f"domain registered: {listed!r}"
    )
    return base / "FileProvider" / "roots" / member_hex.lower() / f"local%3A{folder_id}"


def _ios_tree_listing(udid: str) -> str:
    """Every file under the app groups' `FileProvider` and `share-spool` trees on
    the member's simulator, with its size — what the door landed and where."""
    import os
    from pathlib import Path

    from tests.platform.ios.test_file_provider_eviction import _BUNDLE_ID, _sim

    lines = []
    listed = _sim("get_app_container", udid, _BUNDLE_ID, "groups").stdout
    for entry in listed.splitlines():
        group = Path(entry.split(None, 1)[-1].strip())
        for top in ("FileProvider/roots", "share-spool"):
            for dirpath, _dirs, files in os.walk(group / top):
                for name in files:
                    path = Path(dirpath) / name
                    try:
                        size = path.stat().st_size
                    except OSError:
                        size = -1
                    lines.append(f"{path.relative_to(group)} ({size} B)")
    return "\n".join(lines) or f"<no files under FileProvider/roots or share-spool: {listed!r}>"


def _ios_member_domain(request, nest, owner, member, member_app, set_name, tmp_path):
    """The iOS member's replica: the set's File Provider domain on the seat's own
    simulator, the one the app's foreground plane reaches across to.

    Fixture setup (the writer-revocation iOS arm's precedent): the extension's
    capability is provisioned with the machine principal the seat enrolled before
    its first sign-in (`conftest._folder_share_recipient_seat`) and the domain
    registered under the identity the reconcile would use, through the DEBUG
    `FileProviderTestCLI`. Each verb is a launch of the same bundle and ends the
    seat's process, so the seat's state is pinned and the app relaunched after —
    its `start()` starts the phone plane over the registered domain.

    Returns the staging root (where a peer-landed body rests) and records the
    domain on the driver for the later domain reads.
    """
    import blake3
    import cbor2

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.set_names import find_set
    from tests.platform.ios.test_file_provider_eviction import (
        _BACKUP_KEY_CONTEXT,
        _fauna,
        _keep_logs,
        _register,
    )

    drv = member_app.driver
    udid = drv._launch_config["udid"]
    url = nest["url"]
    member_seed = member["signing_key"].encode()
    member_hex = member["actor_id_hex"]
    writer_secret, device_authorization = member["fp_principal"]

    # A same-nest member keys the set as the nest's own row, `local:<id>`; the
    # owner's `fauna.folders.list` holds the id, found by the set's name hash.
    with WsRpcAdminClient(
        url, actor_id=bytes.fromhex(owner["actor_id_hex"]),
        signing_key=owner["signing_key"].encode(),
    ) as ws:
        rows = ws.call("fauna.folders.list", {})["folders"]
    row = find_set(rows, set_name)
    assert row is not None, f"[owner] the shared set is not listed: {rows!r}"
    folder_id = int(row["id"])
    folder_ref = f"local:{folder_id}"

    device_id = secrets.token_bytes(32)
    with WsRpcAdminClient(url, actor_id=bytes.fromhex(member_hex), signing_key=member_seed) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
        ws.call(
            "fauna.sync.device_grant.register",
            {"device_id": device_id.hex(), "authorization": cbor2.loads(device_authorization)},
        )
    backup_key = blake3.blake3(member_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()

    assert drv.preserve_state_across_relaunch(), "the iOS seat must pin its state"
    assert "provision OK" in _fauna(
        udid, "provision", url, member_hex, device_id.hex(), "fp-ext",
        backup_key.hex(), member["token"], writer_secret.hex(), device_authorization.hex(),
    )
    _register(udid, folder_ref, set_name)

    def _teardown():
        # Kept past the session's device (convention 6): the app's whole log —
        # the pump's DEBUG lines the filtered tail drops — and what the
        # extension's staging tree holds.
        _keep_logs(udid, tmp_path)
        (tmp_path / "member-app.log").write_text(drv.app_stderr_text())
        (tmp_path / "staging-tree.txt").write_text(_ios_tree_listing(udid))
        _fauna(udid, "remove", folder_ref, set_name)
        _fauna(udid, "revoke")

    request.addfinalizer(_teardown)
    drv._share_domain = {"udid": udid, "folder_ref": folder_ref, "set_name": set_name}
    drv.hard_reload()
    return _ios_staging_root(udid, member_hex, folder_id)


def _ios_domain_read(member_app, name: str, expected: str) -> str:
    """Read ``name`` through the member's File Provider domain (`fetchContents`,
    the kept root before the network) and assert it is ``expected``. The verb is
    a launch of the seat's bundle, so it ENDS the seat's process: call it only
    once nothing else is read off the running app, and `_member_back_in_front`
    after."""
    import hashlib

    from tests.platform.ios.test_file_provider_eviction import _extension_diagnostics, _fauna

    dom = member_app.driver._share_domain
    out = _fauna(dom["udid"], "read", dom["folder_ref"], dom["set_name"], name)
    digest = hashlib.sha256(expected.encode()).hexdigest()
    assert f"read OK: {name}" in out and digest in out, (
        f"[member] the domain did not serve {name} byte for byte: {out}\n"
        f"{_extension_diagnostics(dom['udid'])}"
    )
    return out


def _member_back_in_front(member_app) -> None:
    """An iOS seat a domain read ended comes back to the foreground; a no-op elsewhere."""
    if _is_ios(member_app):
        member_app.driver.hard_reload()


def _transfer_surface_readout(driver) -> str:
    """What the six-id surface shows right now, for a failure message (convention 6)."""
    parts = []
    for element_id in ("share-serve-status", "share-transfer-list", "share-transfer-item"):
        try:
            parts.append(f"{element_id}: count={driver.count(element_id)}")
        except Exception as exc:  # a readout must never mask the failure it explains
            parts.append(f"{element_id}: unreadable ({type(exc).__name__})")
    try:
        parts.append(f"serve-status text={driver.get_text('share-serve-status')!r}")
    except Exception as exc:
        parts.append(f"serve-status text unreadable ({type(exc).__name__})")
    # Where the driver can dump its element tree (the apple bridge), say WHY a row
    # reads absent — hidden slot vs. never registered — in the same failure.
    try:
        dump = driver.tree()
        keep, on = [], False
        for line in dump.splitlines():
            if not line.startswith(" "):
                on = line.startswith("share-")
            if on:
                keep.append(line)
        parts.append("tree: " + " | ".join(keep or ["<no share-* entries>"]))
    except Exception:  # no tree on this driver; the counts above stand
        pass
    return "; ".join(parts)


def _await_member_page_clean_after_restart(member_app) -> None:
    """The member's page holds no error, read off a refresh that reached the
    restarted nest.

    A Folders refresh that ran with the nest DOWN honestly committed "Failed to
    load devices: …connection…lost" — a page error that is the nest's absence,
    not the peer pull's, and that clears only on the next refresh to reach the
    restarted nest (whose reconnect backs off up to a minute). So the no-error
    verdict is read off a refresh that BEGAN after the restart and committed
    (convention 14), revisiting until one reaches the nest; an error the pull
    left behind still stands.
    """
    md = member_app.driver
    mb = member_app.backups

    def _member_page_clean() -> bool:
        baseline = devices_refresh_baseline(md)
        md.set_state(_FEED_NAV)
        mb.navigate_folders()
        await_devices_refresh_after(
            md, baseline, budget_s=_FOLDERS_REFRESH_S, what="the post-restart Folders revisit"
        )
        return not member_app.has_error()

    wait_until(
        _member_page_clean,
        _PEER_BARRIER_S,
        interval=2.0,
        diagnose=lambda: (
            "[member] the peer pull surfaced an error that outlived the nest's "
            f"restart: {member_app.error_text()!r}"
        ),
    )


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
            + seat_firewall_note(member_app.driver, "member")
            + _agent_diagnosis(member_app, "member")
            + (
                "\n[member] the iOS staging tree:\n"
                + _ios_tree_listing(member_app.driver._share_domain["udid"])
                if _is_ios(member_app)
                else ""
            )
        ),
    )


def _member_access_or_none(ob, index, row):
    """The member row's access value, or None while the roster re-read that
    follows ``set_member_access`` has the row unpainted (linux rebuilds the
    row, so the read is a LookupError for that beat — a poll tick, not a fail)."""
    try:
        return ob.member_access(index, row=row)
    except LookupError:
        return None


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
        lambda: _member_access_or_none(ob, 0, owner_row) == "writer",
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
        titles = mb.folder_titles()
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

    if _is_ios(member_app):
        # A phone binds no folder: its replica is the set's File Provider
        # domain. `file_1`, sequenced by the nest, lands there as a placeholder
        # row only (a peer body lands only for a row the nest cannot serve —
        # `share_landing::body_is_wanted`), which the barrier below counts.
        member_folder = _ios_member_domain(
            request, nest, _owner, member, member_app, set_name, tmp_path
        )
    else:
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
            # A held inbound flow reads exactly like a dead listener from the
            # dialer's side (`iroh connect: timed out` every pass, both seats):
            # name the firewall's verdict on each seat's binaries when it is ON.
            + seat_firewall_note(md, "member")
            + seat_firewall_note(owner_app.driver, "owner")
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
    _share_plane_seat_params(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (point 9: bounded always): two GUI apps + two real agents +
# a real MLS share + one nest-mediated hydration + a nest stop + a peer pull.
# A ceiling, not an expectation.
@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
def test_member_pulls_offline_authored_file_from_owner_peer(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """A file authored with the nest down arrives peer-to-peer, and the
    transfer surface reports it (rule-5/Dim-3 honesty)."""
    from helpers.app_surface import require_surface

    require_surface(folder_share_recipient_app[0].driver, "share-offline-landing")
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
        # The Folders page renders its transfer rows after navigation, so a
        # single read races the render (convention 14): wait for the row.
        wait_until(
            lambda: md.count("share-transfer-item") >= 1,
            _PEER_BARRIER_S,
            interval=1.0,
            diagnose=lambda: (
                "[member] the transfer surface lost its rows after the peer "
                f"pull that just delivered {file_2}; "
                f"{_transfer_surface_readout(md)}"
            ),
        )
        state = md.get_text("share-transfer-state") or ""
        assert "limited" not in state.lower(), (
            f"[member] the transfer gate refused a pull this test needed: {state!r}"
        )
        if _is_ios(member_app):
            # The phone's file is what Files serves: read through the domain
            # with the nest still down, so the bytes can only be the kept
            # root's (`fetchContents` asks `lookupBody` first). Last, because
            # the read ends the seat's process.
            _ios_domain_read(member_app, file_2, payload_2)
    finally:
        # The session-scoped nest must outlive this test healthy — same port,
        # same data dir, no factory reset.
        start_nest_in_place(nest)

    _member_back_in_front(member_app)
    _await_member_page_clean_after_restart(member_app)


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    _share_plane_seat_params(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (point 9): the step 1–3 ground above plus three peer pulls.
@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
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
    from helpers.app_surface import app_name, require_surface

    require_surface(folder_share_owner_app[0].driver, "share-serve-tally")
    require_surface(folder_share_recipient_app[0].driver, "share-offline-landing")

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

    _await_member_page_clean_after_restart(member_app)


# ── The transfer rows' own readings (share-a-folder outcomes 27–29) ──────────
#
# `p2p-shared-set.md` § The transfer row of a pull that failed, § Wormability
# walk (rule 8, the per-operation cap) and § A departed member's cached
# endpoint: what a `share-transfer-item` row SAYS, as against whether a file
# arrived (the journeys above). Every reading is the shared
# `share_glue::transfer_state_label`, compared against `S.folders`, never a
# literal.


def _transfer_rows(driver) -> list[dict[str, str]]:
    """Every `share-transfer-item` row on the page, each read scoped to itself:
    its name (`folder — who`), its progress and its state."""
    rows = []
    for k in range(driver.count("share-transfer-item")):
        scope = f"share-transfer-item[{k}]"
        row = {}
        for key, element_id in (
            ("name", "share-transfer-name"),
            ("progress", "share-transfer-progress"),
            ("state", "share-transfer-state"),
        ):
            try:
                row[key] = driver.get_text(element_id, scope=scope) or ""
            except LookupError:
                # The row repainted between the count and this read (each pass
                # replaces the cell wholesale): a poll tick, never a fail.
                row[key] = ""
        rows.append(row)
    return rows


def _rows_for(driver, set_name: str) -> list[dict[str, str]]:
    return [r for r in _transfer_rows(driver) if set_name in r["name"]]


def _p2p_share_limits_row(settings) -> int:
    """The Feature limits row index of `p2p-share`, found by its name rather
    than by its place in the registry."""
    for index in range(8):
        try:
            if settings.feature_limit_row(index)["name"] == S.features.name_p2p_share:
                return index
        except LookupError:
            break
    raise AssertionError(
        "the Feature limits section lists no p2p-share row, so no per-file cap "
        "can be set on it from the app"
    )


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    _share_plane_seat_params(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (point 9): the step 1–3 ground above plus one failed dial.
@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
def test_a_pull_from_a_device_that_went_away_keeps_its_row_and_says_why(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """share-a-folder outcome 27: the folder's sync status shows each transfer
    with its progress, and a pull that failed KEEPS its row, reading *Could not
    reach their device* or *Connection lost; retrying* (`p2p-shared-set.md`
    § The transfer row of a pull that failed).

    The barrier leaves the member with a row for the owner's device whose
    progress moved something. The owner then quits the app — the device going
    away is a state, not a race (convention 14): every later pass of the
    member's pump dials an address nothing answers at, and the ruling says that
    pass still yields the row. Which of the two failure readings appears
    depends on how far the cut pull got, so either is the outcome.
    """
    owner_app, member_app, _nest, _owner_folder, _member_folder, set_name = (
        _share_and_reach_peer_barrier(
            request, folder_share_owner_app, folder_share_recipient_app, tmp_path
        )
    )
    md = member_app.driver
    member_app.backups.navigate_folders()

    before = wait_until(
        lambda: _rows_for(md, set_name) or None,
        _PEER_BARRIER_S,
        interval=1.0,
        diagnose=lambda: (
            f"[member] no transfer row names the folder {set_name!r} after the "
            f"barrier; rows={_transfer_rows(md)!r}; {_transfer_surface_readout(md)}"
        ),
    )
    names = {r["name"] for r in before}
    assert all(r["progress"] for r in before), (
        f"[member] every transfer row carries its progress reading: {before!r}"
    )

    # The owner's device goes away. The fixture's own driver stops the app (the
    # user quitting it); its teardown afterwards finds the app already gone.
    owner_app.driver.quit_app()

    failure_readings = {
        S.folders.share_transfer_state_unreachable,
        S.folders.share_transfer_state_interrupted,
    }

    def _failed_row():
        for row in _transfer_rows(md):
            if row["name"] in names and row["state"] in failure_readings:
                return row
        return None

    row = wait_until(
        _failed_row,
        _PEER_TRANSFER_S,
        interval=1.0,
        diagnose=lambda: (
            "[member] with the owner's app quit, no row for the owner's device "
            f"read {sorted(failure_readings)!r}: the rows were {_transfer_rows(md)!r} "
            f"(before the quit: {before!r}). A row that VANISHED is the failure "
            "the ruling removed (a failed pull yielding no outcome); a row still "
            "reading Receiving/Up to date means the member never dialed again. "
            f"{_transfer_surface_readout(md)}; error={member_app.error_text()!r}\n"
            + _agent_diagnosis(member_app, "member")
        ),
    )
    assert row["progress"], (
        f"[member] the failed pull's row keeps its progress reading (this pass's "
        f"counts): {row!r}"
    )


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    _share_plane_seat_params(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (point 9): the step 1–3 ground above, a limit authored in
# Settings, one capped peer pull and one nest-mediated hydration.
@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
def test_a_file_over_the_per_file_cap_arrives_through_the_nest_and_its_row_says_limited(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """share-a-folder outcome 28: a file over the direct-transfer size cap
    arrives through the nest instead, and its transfer row says it was limited
    (`p2p-shared-set.md` § Wormability walk, rule 8: the per-operation cap is
    put to each FILE, a capped file is a permanent spool miss, "the file
    reaches this device by the nest path, and the surface reads 'limited by'
    the cap's tier").

    1. the member sets their OWN per-file cap on Peer transfers to 1 KB, in
       Settings' Feature limits editor (convention 8 — the self tier, the
       limit's source the row will name);
    2. the nest holds every `fauna.sync.changes.list`, so the member's agent
       cannot fetch the new row and the peer pull is the first to meet it —
       without the hold the two paths race, and a row the nest path delivered
       first is never planned peer-side, so it is never capped;
    3. the owner adds a 16 KB file;
    4. the member's row reads "Limited by Your own setting" while the file is
       still absent from the member's folder (kept off the peer leg);
    5. the hold lifts and the file arrives through the nest.

    The limited reading belongs to the pass that capped the file (the cursor
    then moves past its row), so step 4 polls fast enough to read one whole
    pump pass; a loaded box makes a pass longer, never shorter.
    """
    from helpers.app_surface import require_surface
    from helpers.rpc_hold import arm_rpc_hold, release_rpc_hold, rpc_hold_status

    require_surface(folder_share_recipient_app[0].driver, "share-cap-nest-hold")
    owner_app, member_app, nest, owner_folder, member_folder, set_name = (
        _share_and_reach_peer_barrier(
            request, folder_share_owner_app, folder_share_recipient_app, tmp_path
        )
    )
    md = member_app.driver
    settings = member_app.settings
    editor = member_app.feature_policy_editor

    # ── 1. the member's own per-file cap, through the app ──
    settings._navigate_subpage("status")
    settings.wait_for_feature_limits()
    p2p_row = _p2p_share_limits_row(settings)
    settings.open_own_feature_limit_editor(p2p_row)
    editor.wait_open()
    cap_cells = [
        i for i in range(editor.cell_count())
        if editor.cell(i)["label"] == S.features.editor_per_operation_bytes
    ]
    assert len(cap_cells) == 1, (
        "the self-limits editor for Peer transfers carries exactly one "
        f"per-file cap cell; cells={[editor.cell(i) for i in range(editor.cell_count())]!r}"
    )
    editor.set_cell(cap_cells[0], "1 KB")
    editor.save()
    assert editor.wait_for_status(S.features.editor_saved) == S.features.editor_saved, (
        f"[member] the cap never saved: status={editor.status()!r} "
        f"error={member_app.error_text()!r}"
    )
    editor.close_if_open()
    member_app.backups.navigate_folders()

    kind = "fauna.sync.changes.list"
    big = f"over-cap-{secrets.token_hex(3)}.bin"
    payload = ("over the cap — " + secrets.token_hex(8) + "\n") * 512  # ~16 KB
    expected_state = S.folders.share_transfer_state_limited(source=S.features.tier_self)
    try:
        # ── 2–3. hold the nest path, then the owner adds the big file ──
        arm_rpc_hold(nest["port"], kind)
        _atomic_write(owner_folder / big, payload)

        # ── 4. the member's row says limited; the file is not here yet ──
        def _limited_row():
            for row in _rows_for(md, set_name):
                if row["state"] == expected_state:
                    return row
            return None

        wait_until(
            _limited_row,
            _PEER_TRANSFER_S,
            interval=0.25,
            diagnose=lambda: (
                f"[member] no transfer row for {set_name!r} read {expected_state!r} "
                f"after the owner added a {len(payload)}-byte file over the 1 KB "
                f"cap; rows={_transfer_rows(md)!r}; member has the file: "
                f"{_read_or_none(member_folder / big) is not None}. A file that "
                "ARRIVED with no limited reading crossed the peer leg uncapped "
                "(the pass read a policy without the cap, or the cap is not put "
                "to each file); no arrival and no reading means no pass planned "
                f"it (a relayed path defers bodies before the gate). nest hold: "
                f"{rpc_hold_status(nest['port'], kind)!r}; "
                f"error={member_app.error_text()!r}\n"
                + _agent_diagnosis(member_app, "member")
            ),
        )
        assert _read_or_none(member_folder / big) is None, (
            "[member] the over-cap file is already here while the nest path is "
            "held — it crossed the peer leg the cap should have kept it off"
        )
    finally:
        release_rpc_hold(nest["port"], kind)

    # ── 5. the file arrives through the nest ──
    if _is_ios(member_app):
        # A phone's copy of a file the nest serves is the OS's, fetched on
        # open: the domain answers the read through the nest.
        _ios_domain_read(member_app, big, payload)
        return
    wait_until(
        lambda: _read_or_none(member_folder / big) == payload,
        _NEST_HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[member] {big} never arrived through the nest after the hold "
            "lifted — a capped file must still reach the device by the nest "
            f"path; error={member_app.error_text()!r}\n"
            + _agent_diagnosis(member_app, "member")
            + "\n"
            + _agent_diagnosis(owner_app, "owner")
        ),
    )


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    _share_plane_seat_params(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (point 9): the step 1–3 ground above plus one removal.
@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
def test_a_removed_member_drops_off_the_owners_transfer_list(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """share-a-folder outcome 29: a member who left or was removed drops off
    the transfer list, and their devices are no longer contacted
    (`p2p-shared-set.md` § A departed member's cached endpoint, ruling 1: the
    set's roster decides what is dialed, at the dial-target read, so "the
    transfer surface drops the row at the first pass after the roster change,
    by construction").

    The member is a writer whose seat advertises, so the owner's own pump
    pulls from them and the owner's list carries a row for this folder and
    the member's device. The owner removes the member through the folder's
    own controls (convention 8 — the Remove commit lives in the owner's
    engine); from the next pass the owner dials nobody for this folder, so
    no row for it remains. The removed member's app stays running: a row that
    lingered "Could not reach their device" would be the pre-ruling defect.
    """
    owner_app, member_app, _nest, _owner_folder, _member_folder, set_name = (
        _share_and_reach_peer_barrier(
            request, folder_share_owner_app, folder_share_recipient_app, tmp_path
        )
    )
    od = owner_app.driver
    ob = owner_app.backups
    ob.navigate_folders()

    wait_until(
        lambda: _rows_for(od, set_name) or None,
        _PEER_BARRIER_S,
        interval=1.0,
        diagnose=lambda: (
            f"[owner] the owner's list never carried a row for {set_name!r} — "
            "the owner never dialed the member's device, so the removal below "
            "could not be read off the list. The member's seat advertises once "
            "bound as a writer; check the owner's sink. "
            f"rows={_transfer_rows(od)!r}; {_transfer_surface_readout(od)}\n"
            + share_ingest_reading(od, "owner")
            + "\n"
            + _agent_diagnosis(owner_app, "owner")
        ),
    )

    # The expand TOGGLES, and linux keeps the row the share opened open across
    # navigate_folders(), so one toggle can shut it; the member roster also
    # fills asynchronously after the expand. Re-expand until the member's
    # Remove control shows (find_and_expand_folder_until's contract).
    owner_row = ob.find_and_expand_folder_until(
        set_name, "folder-member-remove-button", timeout=60
    )
    ob.remove_shared_member(0, row=owner_row)
    wait_until(
        lambda: ob.shared_member_count() == 0,
        60,
        interval=0.5,
        diagnose=lambda: (
            f"[owner] the removal never landed; error={owner_app.error_text()!r}"
        ),
    )

    ob.navigate_folders()
    wait_until(
        lambda: not _rows_for(od, set_name),
        _PEER_TRANSFER_S,
        interval=1.0,
        diagnose=lambda: (
            f"[owner] the removed member's row for {set_name!r} is still on the "
            f"owner's transfer list: {_rows_for(od, set_name)!r}. The roster no "
            "longer lists them, so the dial-target read must skip their cached "
            f"endpoint; error={owner_app.error_text()!r}\n"
            + _agent_diagnosis(owner_app, "owner")
        ),
    )


#: Apps whose seat carries the share probe (`offline_share_probe_set`, one
#: shared call: `share_probe::probe_set_from_args`). Same joining rule as
#: `_SERVE_HOLD_APPS`.
_PROBE_APPS = ("tui", "linux", "macos", "windows")


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
@pytest.mark.feature("share-a-folder")
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
        f"nothing about the serve door: {stranger_report!r}\n"
        + seat_firewall_note(owner_app.driver, "owner")
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
    # The per-launch state root: tui/linux/macos hold it as `_tmp_dir`; windows
    # relocates `%LOCALAPPDATA%` (the ancestor of its data dir) per launch.
    launch_root = getattr(stranger_driver, "_tmp_dir", None) or getattr(
        stranger_driver, "_local_appdata", None
    )
    assert launch_root, (
        "[stranger] the driver exposes no per-launch state root to scan for "
        "leaked plaintext"
    )
    roots = [launch_root]
    for extra in (
        getattr(stranger_driver, "_resolved_store_root", None),
        getattr(stranger_driver, "_data_dir", None),
    ):
        if extra and not str(extra).startswith(str(launch_root)):
            roots.append(extra)
    leaked = [hit for root in roots for hit in _files_holding(root, secret.encode())]
    assert not leaked, (
        f"[stranger] the payload's plaintext rests on the stranger's device: {leaked}"
    )

    assert not stranger_app.has_error(), (
        f"[stranger] the probe surfaced an app error: {stranger_app.error_text()!r}"
    )
