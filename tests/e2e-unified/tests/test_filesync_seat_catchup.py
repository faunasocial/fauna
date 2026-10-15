"""What an app seat does with changes it did not see arrive live — catch-up,
an un-appliable change, a delete on a backup place.

Owner doc: ``docs/goal/behavior/file-sync.md`` § Technical Flow → *5. Offline
Catch-Up* (a restarted device pulls everything after its anchor; a change it
can never apply is recorded and skipped, never a block) and § 4 (*a backup seat
never applies a peer's delete to its own disk*, the shared
``SyncMode::applies_remote_deletes`` predicate).

These three behaviours were pinned until 2026-10-01 only on the legacy headless
daemon as a reader (``tests/platform/sync/test_catchup_permanent_skip.py`` and
``test_orchestrator.py``), which has since been removed
(``architecture/apps/sync-agent.md`` § Headless deployment). The engine under
test is the same ``fauna-sync-engine``; the seat that runs it is now the tui
app's own ``fauna-sync-agent``, the lead app (``helpers/sync_seats.py``).

The flow every test here asserts::

    the harness writer records a change through the shared engine (signed)
      -> the nest stores the record
      -> the seat's agent pulls it, on the nudge or on its restart
      -> its engine applies it, skips it into a ``catchup_failed`` row (and
         reports that to the nest), or keeps the file on a backup place
      -> the test reads the seat's disk, the nest's ``conflicts.list``, the
         seat's own Folders review list, or its agent log

The writer is ``helpers.harness_writer`` — one signed pass per call, so a
change is on the nest the moment its ``sync()`` returns and the seat's wait
starts from a known write (convention 14: every wait is a deadline poll on
observed state, never a settle sleep).
"""

from __future__ import annotations

import contextlib
import secrets
import shutil
import tempfile
from pathlib import Path

import pytest

from common import register_user, user_create_folder
from common.accounts import actor_id_hex
from common.envelope import CID_RAW_PREFIX, cid_link
from common.auth import (
    UNBINDING_MAX_DEVICES,
    mint_token_via_handshake,
    sealed_path,
    set_tier_caps,
    sync_changes_record,
    sync_conflicts_list,
    sync_devices_list,
    user_folder_ref,
)
from helpers import sync_seats
from helpers.harness_writer import HarnessWriter
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # tui leads (the shared engine runs in every app's agent); the other
    # desktops join through the same `make_seat("native")` seam.
    pytest.mark.tui,
]

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
WINDOW = 180.0


@contextlib.contextmanager
def _writer_and_seat(nest, request, folder: str):
    """A custody-first set, the harness's signed writer, and one tui seat bound
    to it as the same account's second device. Yields ``(writer, seat, secret_key)``.
    """
    # The seat signs in through the fixture-setup carve-out every harness-nest
    # seat uses; one owner, imported rather than copied.
    from tests.test_filesync_seats import handle_for, harness_sign_in

    port, url = nest["port"], nest["url"]
    admin_sk = nest["admin"]["signing_key"]
    secret_key = secrets.token_hex(32)
    actor_id = actor_id_hex(secret_key)
    handle = handle_for(actor_id)
    register_user(port, actor_id, base_url=url, admin_signing_key=admin_sk, handle=handle)
    # The writer and the seat are two devices; never let the tier cap refuse one.
    set_tier_caps(port, admin_signing_key=admin_sk, max_devices=UNBINDING_MAX_DEVICES,
                  base_url=url)
    # Custody-first: the writer signs under the nonce custody holds.
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    try:
        writer = HarnessWriter(
            url, secret_key,
            user_folder_ref(port, folder, secret_key=secret_key, base_url=url),
            tmp / "writer",
        )
        seat = sync_seats.make_seat(
            "tui",
            name="b",
            run_token=sync_seats.new_run_token(seats=1),
            root=tmp / "b",
            node_url=url,
            node_port=port,
            folder=folder,
            sign_in=harness_sign_in(url, secret_key, actor_id, handle),
            request=request,
        )
        with seat:
            seat.await_ready(STARTUP_WINDOW)
            yield writer, seat, secret_key
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _await_bytes(seat, rel: str, body: bytes, what: str) -> None:
    target = seat.path / rel

    def landed() -> bool:
        try:
            return target.read_bytes() == body
        except OSError:
            return False

    wait_until(landed, WINDOW, interval=1.0,
               diagnose=lambda: f"{what}: {rel} never reached the seat\n{seat.diagnostics()}")


def _await_log(seat, needle: str, what: str, *, after: int = 0) -> int:
    """Wait for a line containing ``needle`` past the first ``after`` lines of
    the seat's agent log; return the log's length at the hit."""
    hit = {}

    def seen() -> bool:
        lines = seat.agent_log_lines()
        if any(needle in ln for ln in lines[after:]):
            hit["n"] = len(lines)
            return True
        return False

    wait_until(seen, WINDOW, interval=1.0,
               diagnose=lambda: f"{what}: no {needle!r} in the agent log\n{seat.diagnostics()}")
    return hit["n"]


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * WINDOW + 120))
@pytest.mark.feature("local-folder-sync")
def test_a_restarted_seat_receives_what_it_missed(nest_instance, request):
    """The seat is stopped, misses a write, and receives it on restart — catch-up
    from a non-zero anchor, the same device, its state kept."""
    with _writer_and_seat(nest_instance, request, "replay-docs") as (writer, seat, _):
        # Baseline through the live path: the seat's anchor is demonstrably
        # past zero, so what the restart does is catch-up, not first sync.
        writer.write("file1.txt", "first file")
        writer.sync(expect=["file1.txt"])
        _await_bytes(seat, "file1.txt", b"first file", "baseline")

        seat.stop_keeping_state()
        # `sync()` returns once the record and its bytes are on the nest, and
        # the seat is not running — so this change can only arrive by catch-up.
        writer.write("file2.txt", "second file")
        writer.sync(expect=["file2.txt"])

        seat.restart(STARTUP_WINDOW)
        _await_bytes(seat, "file2.txt", b"second file", "catch-up after restart")


def _upload(base: str, token: str, route: str, body: bytes,
            content_hash: bytes | None = None) -> str:
    import json
    import urllib.request

    req = urllib.request.Request(
        f"{base}{route}",
        data=body,
        method="POST",
        headers={
            "Content-Type": "application/octet-stream",
            "Authorization": f"Bearer {token}",
            **({"X-Content-Hash": content_hash.hex()} if content_hash is not None else {}),
        },
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())["hash"]


def _upload_chunk_and_manifest(base: str, token: str, data: bytes, *,
                               addresses: bytes | None = None) -> str:
    """Upload ``data`` as one framed chunk plus a single-chunk manifest; return
    the manifest's hash.

    ``addresses`` overrides the manifest's whole-file content address: passed
    the digest of OTHER bytes, the chunk still addresses its own entry (so the
    transport stage downloads it and the per-chunk check passes) and only the
    whole-file compare fails — the doc's *"content that does not address its
    recorded hash"*, a PERMANENT class. A transient fault would correctly stop
    the pass and never reach the change behind it, so this test must not use
    one (`file-sync.md` § 5, transient is the default).

    Hand-built because the FFI manifest record cannot express an inconsistent
    one; ``cbor2.dumps(..., canonical=True)`` is byte-identical to
    ``encode_canonical``. The chunk goes up framed (``0x00`` ‖ data, the
    uncompressed frame every writer produces), keyed by ``blake3(data)``.
    """
    import blake3
    import cbor2

    chunk_digest = blake3.blake3(data).digest()
    stored = _upload(base, token, "/api/v1/chunks", b"\x00" + data, chunk_digest)
    assert stored == chunk_digest.hex(), (stored, chunk_digest.hex())
    manifest = {
        "file_hash": cid_link(
            CID_RAW_PREFIX + (addresses if addresses is not None else chunk_digest)
        ),
        "total_size": len(data),
        "chunk_hashes": [cid_link(CID_RAW_PREFIX + chunk_digest)],
        "chunk_sizes": [len(data)],
    }
    return _upload(base, token, "/api/v1/manifests", cbor2.dumps(manifest, canonical=True))


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * WINDOW + 120))
@pytest.mark.feature("local-folder-sync")
def test_a_permanently_unappliable_change_is_recorded_and_never_blocks_the_rest(
    nest_instance, request
):
    """Two stuck changes ahead of a good one in the catch-up sequence: the good
    one lands, the stuck ones never reach the disk, and each is REPORTED — on
    the nest's ``conflicts.list`` and the seat's own review list as a *Not
    applied* row naming the device that is behind (name-less for the one whose
    sealed path never opens) — until a later version of the file cures it
    (`conflicts.md` § Skipped catch-up changes reach the review list).

    Measured live 2026-07-31: one un-appliable change froze a device's anchor
    and it pulled zero of the changes after it (`file-sync.md` § 5).
    """
    import blake3
    from nacl.signing import SigningKey

    folder = "catchup-skip-docs"
    stuck_path, later_path = "stuck.bin", "later.txt"
    later_body = b"the changes after the stuck one keep arriving"
    port, base = nest_instance["port"], nest_instance["url"]

    with _writer_and_seat(nest_instance, request, folder) as (writer, seat, secret_key):
        writer.write("first.txt", "baseline")
        writer.sync(expect=["first.txt"])
        _await_bytes(seat, "first.txt", b"baseline", "baseline")

        # The rows go in while the seat is down, so both arrive by catch-up and
        # in seq order.
        seat.stop_keeping_state()
        token = mint_token_via_handshake(base, SigningKey(bytes.fromhex(secret_key)))
        device = writer.device_id.hex()

        # The stuck row FIRST: what broke live was the anchor freezing below it.
        stuck = sync_changes_record(
            port, secret_key=secret_key, folder=folder, device_id=device,
            path=stuck_path, size_bytes=44, base_url=base,
            manifest_hash=_upload_chunk_and_manifest(
                base, token, b"these bytes are not what the manifest claims",
                addresses=blake3.blake3(b"a different file entirely").digest(),
            ),
        )
        # A change whose sealed path this seat can never open — the name-less
        # skip, also ahead of the good change.
        unreadable = sync_changes_record(
            port, secret_key=secret_key, folder=folder, device_id=device,
            path="unreadable.bin", size_bytes=len(later_body), base_url=base,
            path_sealed=_UNOPENABLE_SEAL,
            manifest_hash=_upload_chunk_and_manifest(base, token, later_body),
        )
        later = sync_changes_record(
            port, secret_key=secret_key, folder=folder, device_id=device,
            path=later_path, size_bytes=len(later_body), base_url=base,
            manifest_hash=_upload_chunk_and_manifest(base, token, later_body),
        )
        assert stuck["seq"] < unreadable["seq"] < later["seq"], (
            f"the good change must sit behind both stuck ones: {stuck=} "
            f"{unreadable=} {later=}"
        )

        seat.restart(STARTUP_WINDOW)

        # THE assertion: catch-up applies in seq order, so this landing proves
        # the stuck row ahead of it was decided and the anchor moved past it.
        _await_bytes(seat, later_path, later_body, "the change behind the stuck one")
        # A bare absence is safe only behind that barrier: the pass that would
        # have written it has provably run.
        assert not (seat.path / stuck_path).exists(), (
            "a change whose content does not address its recorded hash must never "
            "reach the user's disk"
        )

        # Recorded, not silent — and REPORTED: the seat files each skip on the
        # nest as an unresolved, candidate-free `catchup_failed` conflict that
        # names the seat itself (`conflicts.md` § Skipped catch-up changes reach
        # the review list). Matched by `path_sealed`, the row's only label: the
        # stuck change's under the real funnel, the sealed-path change's
        # forwarded verbatim.
        stuck_seal = sealed_path(secret_key, stuck_path)
        skips = _await_skip_reports(nest_instance, secret_key, writer,
                                    {stuck_seal, _UNOPENABLE_SEAL}, seat)
        seat_device = skips[stuck_seal]["device_id"]
        assert skips[_UNOPENABLE_SEAL]["device_id"] == seat_device, skips

        # The user sees them: the seat's own Folders page lists both as
        # informational *Not applied* rows naming the device that is behind —
        # the readable one by its path, the sealed one name-less, never dropped.
        label = _device_label(nest_instance, secret_key, seat_device)
        set_rows = _await_not_applied_rows(seat, folder, 2)
        assert sorted(info for _, info in set_rows) == sorted([
            f"{folder}: {stuck_path} ({label})",
            f"{folder}: {S.devices.conflicts.unreadable_path} ({label})",
        ]), f"the review list's Not applied rows: {set_rows!r}\n{seat.diagnostics()}"

        # ── The cure: any later version of the file applies normally and the
        # row clears on its own — off the nest's unresolved list and off the
        # page — while the sealed-path skip, which nothing cured, stays. ──
        writer.write(stuck_path, "a version this device can apply")
        writer.sync(expect=[stuck_path])
        _await_bytes(seat, stuck_path, b"a version this device can apply", "the cure")
        wait_until(
            lambda: set(_unresolved_skips(nest_instance, secret_key, writer))
            == {_UNOPENABLE_SEAL},
            WINDOW, interval=1.0,
            diagnose=lambda: (
                "the applied later change never resolved the skip on the nest: "
                f"{_unresolved_skips(nest_instance, secret_key, writer)!r}\n"
                f"{seat.diagnostics()}"
            ),
        )
        set_rows = _await_not_applied_rows(seat, folder, 1)
        assert [info for _, info in set_rows] == [
            f"{folder}: {S.devices.conflicts.unreadable_path} ({label})"
        ], f"a cured skip must leave the review list: {set_rows!r}\n{seat.diagnostics()}"


# A `path_sealed` no key opens: not a decodable `SealedLabel` at all, the
# `Envelope` refusal (`fauna_core::label_custody::ChangePathRefusal`) — a
# PERMANENT class decided before any key is consulted, so the seat skips it at
# once instead of deferring it as key material still syncing (`NoRoot`). The
# nest copies the blob it cannot check.
_UNOPENABLE_SEAL = b"\xffthis is no sealed label"


def _unresolved_skips(nest, secret_key: str, writer) -> dict[bytes, dict]:
    """The nest's unresolved ``catchup_failed`` rows filed by a device other
    than the writer, keyed by ``path_sealed``."""
    rows = sync_conflicts_list(nest["port"], secret_key=secret_key,
                               base_url=nest["url"])["conflicts"]
    return {
        bytes(c.get("path_sealed") or b""): c
        for c in rows
        if c["conflict_type"] == "catchup_failed"
        and c["device_id"] != writer.device_id.hex()
    }


def _await_skip_reports(nest, secret_key: str, writer, seals: set, seat) -> dict:
    """Wait for one unresolved ``catchup_failed`` report per seal; check its
    shape (candidate-free, unresolved) and return them keyed by seal."""
    got: dict = {}

    def reported() -> bool:
        got.clear()
        got.update(_unresolved_skips(nest, secret_key, writer))
        return seals <= set(got)

    wait_until(reported, WINDOW, interval=1.0,
               diagnose=lambda: f"the seat's skips never reached conflicts.list: "
                                f"{got!r}\n{seat.diagnostics()}")
    assert set(got) == seals, f"exactly the two skipped changes are reported: {got!r}"
    for row in got.values():
        assert not row.get("candidates") and row.get("resolved_at") is None, row
    return got


def _device_label(nest, secret_key: str, device_hex: str) -> str:
    """The device's name as every app renders it (the sealed label, opened)."""
    from fauna_ffi import open_device_label

    row = next(d for d in sync_devices_list(nest["port"], secret_key=secret_key,
                                            base_url=nest["url"])["devices"]
               if d["device_id"] == device_hex)
    return open_device_label(bytes.fromhex(secret_key), bytes.fromhex(device_hex),
                             row.get("label_sealed"), row.get("label", ""))


def _await_not_applied_rows(seat, folder: str, count: int) -> list:
    """The seat's Folders-page review rows for ``folder`` badged *Not applied*,
    once exactly ``count`` of them render (the page reloads on every read)."""
    got: list = []

    def rendered() -> bool:
        got[:] = [
            (badge, info) for badge, info in (seat.review_rows() or [])
            if badge == S.devices.conflicts.type_catchup_failed
            and info.startswith(f"{folder}: ")
        ]
        return len(got) == count

    wait_until(rendered, WINDOW, interval=2.0,
               diagnose=lambda: f"expected {count} Not applied row(s) for {folder}, "
                                f"read {got!r}\n{seat.diagnostics()}")
    return got


@pytest.mark.timeout(int(STARTUP_WINDOW + 4 * WINDOW + 120))
@pytest.mark.feature("local-folder-sync")
def test_a_backup_seat_keeps_a_file_the_source_deleted(nest_instance, request):
    """A seat whose place does not apply deletes keeps the file after the source
    deletes it — whatever rail delivered the delete (`file-sync.md` § 4)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from nacl.signing import SigningKey

    folder = "backup-docs"
    with _writer_and_seat(nest_instance, request, folder) as (writer, seat, secret_key):
        sk = SigningKey(bytes.fromhex(secret_key))
        client = WsRpcAdminClient(nest_instance["url"], actor_id=bytes(sk.verify_key),
                                  signing_key=bytes(sk))

        def my_place() -> tuple[int, dict]:
            with client:
                members = client.call("fauna.folders.members.list", {"name": folder})["members"]
            # The writer's pass seats no place; the bind seated this one.
            return next((i, m) for i, m in enumerate(members)
                        if m["device_id"] != writer.device_id.hex())

        writer.write("important.txt", "do not lose this")
        writer.sync(expect=["important.txt"])
        _await_bytes(seat, "important.txt", b"do not lose this", "baseline")

        # ── Make this seat the archive point, through the app's own place
        # editor (convention 8) ──
        driver, b = seat._driver, seat._app.backups
        b.navigate_folders()
        b.find_and_expand_folder_until(folder, "folder-place-row")
        index, _ = my_place()
        scope = f"folder-place-row[{index}]"
        driver.click("folder-place-applies-deletes", scope=scope)
        wait_until(
            lambda: my_place()[1]["flags"]["applies_deletes"] is False, 30.0,
            diagnose=lambda: f"the click never reached the nest: {my_place()!r} "
                             f"error={seat._app.error_text()!r}",
        )
        # The RUNNING engine must have re-resolved its role before the delete is
        # written, or the delete races the flag — its own log line is the barrier.
        mark = _await_log(seat, "Resolved(Backup)", "the seat's engine took the backup role")

        writer.path.joinpath("important.txt").unlink()
        writer.sync()

        # The positive witness: the engine received the delete and declined it.
        _await_log(seat, "backup mode: keeping the local copy", "the delete arrived",
                   after=mark)
        assert (seat.path / "important.txt").read_bytes() == b"do not lose this", (
            "a backup place must keep a file its source deleted"
        )
