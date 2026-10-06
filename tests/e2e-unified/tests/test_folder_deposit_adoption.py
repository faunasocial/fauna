"""tier_3 e2e: **third-party deposit adoption** — the owner's seat opens a
parked deposit and makes it a file like any other
(``docs/goal/behavior/file-sync.md`` § Third-party deposit ingress, the
adoption paragraph; the write door, § Content-Addressed Storage → *Write-path
containment*).

One journey, two tui seats of the same account (the owner's laptop and
tablet), over a real consent ceremony:

- the owner's folder is bound on both seats; the owner enables mail on seat
  a's own Mail settings page, which mints the MSEK custody adoption opens with
  and publishes the recipient key the nest seals deposits to;
- a connected app holding ``fauna:folder:deposit:<folder>`` deposits one file
  through S9a's door (``fauna.folders.deposit`` on its principal session);
- the seats' next catch-up adopts it: the file lands on BOTH seats' disks
  under its own name, exactly once — one seat adopts, the other finds it
  landed (or both land the same bytes, which converge to one file);
- the inbox segment drains, and the file's bytes rest nowhere in clear on the
  nest's disk: the adopted copy is sealed under the folder's own scheme.

The deposit door itself is ``tests/api/test_folder_deposit.py``'s; this
journey reuses its consent helpers rather than copying them. Only an owner's
seat can open a parked item (it is sealed to the owner's recipient key), so
a member's view of an adopted file in a bound folder is the ordinary
shared-folder path, not this journey's.
"""

from __future__ import annotations

import contextlib
import shutil
import tempfile
from pathlib import Path

import pytest
from cryptography.hazmat.primitives.asymmetric import x25519

from common.accounts import actor_id_hex
from common.auth import UNBINDING_MAX_DEVICES, set_tier_caps, user_create_folder
from helpers import sync_seats
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.client_metadata_server import client_document
from helpers.oauth_client import OAuthClient
from helpers.waiting import wait_until
from tests.api.test_folder_deposit import (  # noqa: F401 — fixtures adopted by import
    _approve_with_grant,
    _everything_on_disk,
    _inbox,
    _raw,
    consent_nest_env,
    metadata_server,
)
from tests.api.test_third_party_session import _Session, _upgrade

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

DOCUMENT = "/adoption-client.json"
REDIRECT_URI = "http://127.0.0.1:17777/callback"
NAME = "deposited-report-e1.txt"
MARKER = b"adopted-deposit-plaintext-4b9d"

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
WINDOW = 180.0


@contextlib.contextmanager
def _owner_seats(nest, request, folder: str):
    """The consent nest's owner on two tui seats bound to ``folder`` — the same
    account, two devices. Yields ``(seat_a, seat_b)``."""
    from tests.test_filesync_seats import harness_sign_in

    alice = nest["user"]
    secret_key = bytes(alice["signing_key"]).hex()
    actor_id = actor_id_hex(secret_key)
    set_tier_caps(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES, base_url=nest["url"],
    )
    run_token = sync_seats.new_run_token(seats=2)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    try:
        with contextlib.ExitStack() as stack:
            seats = [
                stack.enter_context(
                    sync_seats.make_seat(
                        "tui",
                        name=name,
                        run_token=run_token,
                        root=tmp / name,
                        node_url=nest["url"],
                        node_port=nest["port"],
                        folder=folder,
                        sign_in=harness_sign_in(
                            nest["url"], secret_key, actor_id, f"alice.{HANDLE_DOMAIN}"
                        ),
                        request=request,
                    )
                )
                for name in ("a", "b")
            ]
            for seat in seats:
                seat.await_ready(STARTUP_WINDOW)
            yield seats[0], seats[1]
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _user_files(seat) -> list[str]:
    """The names in the seat's folder — the engine's own dot-dirs aside."""
    return sorted(p.name for p in seat.path.iterdir() if not p.name.startswith("."))


def _await_landed(seat, what: str) -> None:
    target = seat.path / NAME

    def landed() -> bool:
        try:
            return target.read_bytes() == MARKER
        except OSError:
            return False

    wait_until(
        landed, WINDOW, interval=1.0,
        diagnose=lambda: (
            f"{what}: the deposit never landed at {target}; folder holds "
            f"{_user_files(seat)!r}\n{seat.diagnostics()}"
        ),
    )


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * WINDOW + 240))
@pytest.mark.feature("connected-apps")
def test_an_owner_seat_adopts_a_parked_deposit_once(
    consent_nest, consent_bridge, metadata_server, request
):
    nest = consent_nest
    alice = nest["user"]
    folder = f"drop-{sync_seats.new_run_token(seats=1)[-8:]}"
    folder_id = user_create_folder(
        nest["port"], folder, secret_key=bytes(alice["signing_key"]).hex(),
        base_url=nest["url"],
    )["id"]

    with _owner_seats(nest, request, folder) as (seat_a, seat_b):
        # The owner's mailbox, enabled the way a user enables one: the MSEK
        # custody both seats' engines open deposits with, and the recipient
        # key the nest seals them to.
        mail = seat_a.app.mail_settings
        mail.navigate()
        mail.ensure_mail_enabled()

        scope = f"fauna:folder:deposit:{folder_id}"
        metadata_server.documents[DOCUMENT] = client_document(
            metadata_server.client_id(DOCUMENT), REDIRECT_URI, scope
        )
        holder = x25519.X25519PrivateKey.generate()
        holder_pub = _raw(holder.public_key())
        client = OAuthClient(
            base=nest["url"],
            htu_origin=f"https://{HANDLE_DOMAIN}",
            redirect_uri=REDIRECT_URI,
            scope=scope,
            holder_x25519=holder_pub,
            client_id_url=metadata_server.client_id(DOCUMENT),
        )
        token = _approve_with_grant(nest, client, holder_pub, folder_id)["access_token"]
        session = _Session(_upgrade(nest, client, token))
        try:
            ok, reply = session.call(
                "fauna.folders.deposit",
                {"folder_id": folder_id, "name": NAME, "content_type": "text/plain",
                 "body": MARKER},
            )
        finally:
            session.ws.close()
        assert ok and reply == {"accepted": True}, f"the deposit door refused: {reply!r}"

        # ── Adoption: on both seats, once. ──
        _await_landed(seat_a, "seat a")
        _await_landed(seat_b, "seat b")
        wait_until(
            lambda: _inbox(nest, folder_id) == [], WINDOW, interval=1.0,
            diagnose=lambda: (
                f"the inbox never drained: {len(_inbox(nest, folder_id))} item(s) "
                f"still parked\n{seat_a.diagnostics()}\n{seat_b.diagnostics()}"
            ),
        )
        for seat in (seat_a, seat_b):
            assert _user_files(seat) == [NAME], (
                f"[{seat.name}] one adopted file, never a duplicate: {_user_files(seat)!r}"
            )

    # ── At rest: sealed under the folder's scheme, nothing in clear. ──
    for path, raw in _everything_on_disk(nest):
        assert MARKER not in raw, (
            f"the adopted file's bytes rest in cleartext in {path} — adoption must "
            "re-seal under the folder's own scheme (file-sync.md § Third-party "
            "deposit ingress)"
        )
