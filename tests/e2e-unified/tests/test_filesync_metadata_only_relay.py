"""Two devices pass a file through a **metadata-only** folder — the folder whose
content never rests on the nest.

Owner: ``docs/goal/behavior/file-sync.md`` § Relay serving. A metadata-only
folder's bytes live on the user's devices alone, so a second device gets a file
only by asking the nest to relay it from a device that holds it: the holder's
sync agent has announced the folder on its own connection, the nest asks it for
each chunk, and the agent answers from the file on disk.

**What this pins that the tier below cannot.** The two-engine conformance test
(``bins/fauna-nest/tests/conformance_relay_serving_two_engines.rs``) drives two
engines and the shared seat by hand and names the chunks the store must not
hold. This test is the same flow through the shipped processes: two real apps,
each with its own sync agent, and nothing driving the seat but the agent's own
wiring. If the agent never announced, or never routed an ask to its engine, the
second device would list the file and never receive it — which is exactly what
every app-only deployment did before the serving seat existed.

Two seats on one machine, no operator (convention 16). Both are devices of one
account; seat ``a`` writes, seat ``b`` is started only after the record has
landed, so everything it receives arrives by relay.
"""

import contextlib
import secrets
import shutil
import tempfile
from pathlib import Path

import pytest

from common import register_user, set_folder_residency, sync_status, user_create_folder
from common.accounts import actor_id_hex
from common.auth import UNBINDING_MAX_DEVICES, _user_call, set_tier_caps
from helpers import sync_seats
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # tui leads (the shared engine runs in every desktop app's agent); the other
    # desktops join through the same `make_seat("native")` seam.
    pytest.mark.tui,
]

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
WINDOW = 180.0


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * WINDOW + 120))
@pytest.mark.feature("local-folder-sync")
def test_a_second_device_receives_a_metadata_only_folders_file_from_the_first(
    nest_instance, request
):
    """Seat ``a`` writes a file into a metadata-only folder; seat ``b``, started
    afterwards, receives its bytes — which only ``a``'s agent could have served."""
    from tests.test_filesync_seats import handle_for, harness_sign_in

    port, url = nest_instance["port"], nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    secret_key = secrets.token_hex(32)
    actor_id = actor_id_hex(secret_key)
    handle = handle_for(actor_id)
    register_user(port, actor_id, base_url=url, admin_signing_key=admin_sk, handle=handle)
    # Two seats are two devices; never let the tier cap refuse one.
    set_tier_caps(port, admin_signing_key=admin_sk, max_devices=UNBINDING_MAX_DEVICES,
                  base_url=url)
    folder = f"relay-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    # Metadata-only BEFORE any engine first sees the folder, so no seat ever
    # uploads these bytes and the relay is the only way they can move.
    reply = set_folder_residency(
        port, folder, secret_key=secret_key, residency="metadata_only", base_url=url
    )
    assert reply.get("ok") is True, f"residency set refused: {reply!r}"

    run_token = sync_seats.new_run_token(seats=2)
    name = f"{run_token}-ledger.txt"
    body = f"bytes that live on the first device alone {secrets.token_hex(16)}\n".encode()
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))

    def seat(seat_name: str):
        return sync_seats.make_seat(
            "tui",
            name=seat_name,
            run_token=run_token,
            root=tmp / seat_name,
            node_url=url,
            node_port=port,
            folder=folder,
            sign_in=harness_sign_in(url, secret_key, actor_id, handle),
            request=request,
        )

    def listed() -> list:
        items = _user_call(
            port, secret_key, "fauna.media.list", {"limit": 1000, "cursor_version": 2}, url
        ).get("items", [])
        return [item for item in items if item.get("folder") == folder]

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            a.await_ready(STARTUP_WINDOW)
            (a.path / name).write_bytes(body)
            wait_until(
                lambda: len(listed()) == 1, WINDOW, interval=1.0,
                diagnose=lambda: f"seat a never recorded {name!r}\n{a.diagnostics()}",
            )
            # The causal barrier for seat b's read: the nest counts seat a's
            # agent as a holder only once its announce is admitted.
            wait_until(
                lambda: sync_status(
                    port, secret_key=secret_key, folder=folder, base_url=url
                ).get("source_online") is True,
                WINDOW, interval=1.0,
                diagnose=lambda: (
                    "seat a's agent never announced the folder, so the nest has no "
                    f"device to ask for its bytes\n{a.diagnostics()}"
                ),
            )

            b = stack.enter_context(seat("b"))
            b.await_ready(STARTUP_WINDOW)
            target = b.path / name

            def landed() -> bool:
                try:
                    return target.read_bytes() == body
                except OSError:
                    return False

            wait_until(
                landed, WINDOW, interval=1.0,
                diagnose=lambda: (
                    f"{name!r} never reached seat b byte for byte — its folder is "
                    "metadata-only, so the bytes can only have come from seat a's "
                    f"agent by relay\n--- seat b ---\n{b.diagnostics()}\n"
                    f"--- seat a ---\n{a.diagnostics()}"
                ),
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
