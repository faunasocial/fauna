"""tier_3 e2e: **an on-demand seat adopts a third-party deposit** — the linux
FUSE root runs the same adoption pass the always-resident root runs
(``docs/goal/behavior/file-sync.md`` § Third-party deposit ingress, the
adoption paragraph; the root, ``on-demand-files.md`` § Linux FUSE binding).

One journey, one linux app seat of the folder's owner, over a real consent
ceremony:

- the owner's folder is bound on the seat, and the owner turns that binding
  on-demand with the bound row's switch: the agent mounts its FUSE root over
  the directory (the kernel's mount table is the witness);
- the owner enables mail on the seat's Mail settings page, which mints the
  MSEK custody adoption opens with and publishes the recipient key the nest
  seals deposits to;
- a connected app holding ``fauna:folder:deposit:<folder>`` deposits one file
  through the deposit door;
- the on-demand root's next pass adopts it: ANOTHER process reads the file
  at its own name through the mount, the inbox drains, and the bytes rest
  nowhere in clear on the nest's disk;
- the switch back unmounts, and the directory is an ordinary directory
  holding the adopted file.

The always-resident twin is ``test_folder_deposit_adoption.py`` (two tui
seats); this module reuses its consent helpers the same way it reuses the
deposit door's. The loop's own arms (a pass after the startup fold and after
every re-pull, the retired file flipped in-sync) are pinned by the hydration
loop's unit tests in ``bins/fauna-sync-agent/src/bridge.rs``; this journey
pins that the APP's on-demand switch reaches an agent whose root adopts.

The seat's directory sits under ``/tmp`` whatever ``TMPDIR`` says
(``helpers.folder_content.mountable_location``'s reason: Ubuntu's AppArmor
profile for ``fusermount3`` admits a mount point only under a short list).
"""

from __future__ import annotations

import shutil
import subprocess
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
from helpers.folder_content import _log_path, on_demand_mount_stands
from helpers.fuse_mount import release_mount, run_through_mount
from helpers.oauth_client import OAuthClient
from helpers.principal_session import PrincipalSession, upgrade
from helpers.waiting import wait_until
from tests.api.test_folder_deposit import (  # noqa: F401 — fixtures adopted by import
    _approve_with_grant,
    _everything_on_disk,
    _inbox,
    _raw,
    consent_nest_env,
    metadata_server,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

DOCUMENT = "/adoption-on-demand-client.json"
REDIRECT_URI = "http://127.0.0.1:17778/callback"
NAME = "deposited-on-demand-e1.txt"
MARKER = b"adopted-on-demand-deposit-plaintext-7c2e"

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
WINDOW = 180.0
MOUNT_WINDOW = 90.0


def _run(seat, *argv: str) -> subprocess.CompletedProcess:
    """One short child process through the seat's mount — the 'another
    process' of every read. Bounded even when the agent behind the mount is
    dead (``helpers.fuse_mount``): a hung FUSE request fails the test naming
    the seat's state, never wedges the run."""
    return run_through_mount(seat.path, *argv, diagnose=seat.diagnostics)


def _await_mount(seat, *, mounted: bool) -> None:
    """Poll the kernel's mount table until the on-demand root is (or is no
    longer) mounted over the seat's directory — a state, read from outside the
    agent."""
    wait_until(
        lambda: on_demand_mount_stands(seat.path) == mounted, MOUNT_WINDOW, interval=0.3,
        diagnose=lambda: (
            f"the on-demand root was {'never mounted over' if mounted else 'never unmounted from'} "
            f"{seat.path}; error={seat.app.error_text()!r}\n{seat.diagnostics()}"
        ),
    )


def _read_through_mount(seat) -> bytes | None:
    """The deposit's bytes as another process reads them at the user's path."""
    read = _run(seat, "cat", str(seat.path / NAME))
    return read.stdout if read.returncode == 0 else None


@pytest.mark.timeout(int(STARTUP_WINDOW + 3 * WINDOW + 2 * MOUNT_WINDOW + 240))
@pytest.mark.feature("connected-apps")
def test_an_on_demand_seat_adopts_a_parked_deposit(
    consent_nest, consent_bridge, metadata_server, request
):
    from tests.test_filesync_seats import harness_sign_in

    nest = consent_nest
    alice = nest["user"]
    secret_key = bytes(alice["signing_key"]).hex()
    folder = f"drop-od-{sync_seats.new_run_token(seats=1)[-8:]}"
    folder_id = user_create_folder(
        nest["port"], folder, secret_key=secret_key, base_url=nest["url"],
    )["id"]
    set_tier_caps(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES, base_url=nest["url"],
    )

    tmp = Path(tempfile.mkdtemp(prefix="fauna-e2e-deposit-od-", dir="/tmp"))
    try:
        with sync_seats.make_seat(
            "native",
            name="a",
            run_token=sync_seats.new_run_token(seats=1),
            root=tmp / "a",
            node_url=nest["url"],
            node_port=nest["port"],
            folder=folder,
            sign_in=harness_sign_in(
                nest["url"], secret_key, actor_id_hex(secret_key), f"alice.{HANDLE_DOMAIN}"
            ),
            request=request,
        ) as seat:
            seat.await_ready(STARTUP_WINDOW)
            app = seat.app

            # MUTATION (UI): the bound row's switch turns the folder on-demand.
            b = app.backups
            b.navigate_folders()
            scope = f"folder-row[{b.open_folder(folder)}]"
            loc = app.sync_locations
            assert loc.mode_toggle_visible(scope=scope), (
                "the bound row must render folder-location-mode-toggle; "
                f"{app.driver.diagnose('folder-location-mode-toggle')} "
                f"error={app.error_text()!r}"
            )
            assert loc.toggle_mode(scope=scope) == "on-demand", (
                f"the switch did not move the row on-demand; error={app.error_text()!r}\n"
                f"{seat.diagnostics()}"
            )
            _await_mount(seat, mounted=True)

            # The owner's mailbox, enabled the way a user enables one.
            mail = app.mail_settings
            mail.navigate()
            mail.ensure_mail_enabled()

            # The third party's deposit, through the door on its principal session.
            scope_str = f"fauna:folder:deposit:{folder_id}"
            metadata_server.documents[DOCUMENT] = client_document(
                metadata_server.client_id(DOCUMENT), REDIRECT_URI, scope_str
            )
            holder = x25519.X25519PrivateKey.generate()
            holder_pub = _raw(holder.public_key())
            client = OAuthClient(
                base=nest["url"],
                htu_origin=f"https://{HANDLE_DOMAIN}",
                redirect_uri=REDIRECT_URI,
                scope=scope_str,
                holder_x25519=holder_pub,
                client_id_url=metadata_server.client_id(DOCUMENT),
            )
            token = _approve_with_grant(
                nest, client, holder_pub, folder_id, scope_str, choose=False
            )["access_token"]
            session = PrincipalSession(upgrade(nest, client, token))
            try:
                ok, reply = session.call(
                    "fauna.folders.deposit",
                    {"folder_id": folder_id, "name": NAME, "content_type": "text/plain",
                     "body": MARKER},
                )
            finally:
                session.ws.close()
            assert ok and reply == {"accepted": True}, f"the deposit door refused: {reply!r}"

            # ── Adoption on the on-demand root, read by another process. ──
            wait_until(
                lambda: _read_through_mount(seat) == MARKER, WINDOW, interval=1.0,
                diagnose=lambda: (
                    f"the deposit never became readable at {seat.path / NAME} through "
                    f"the on-demand mount; `ls` reads "
                    f"{_run(seat, 'ls', '-1', str(seat.path)).stdout!r}\n{seat.diagnostics()}"
                ),
            )
            wait_until(
                lambda: _inbox(nest, folder_id) == [], WINDOW, interval=1.0,
                diagnose=lambda: (
                    f"the inbox never drained: {len(_inbox(nest, folder_id))} item(s) "
                    f"still parked\n{seat.diagnostics()}"
                ),
            )
            # The engine's own witness: the agent's on-demand root — the only
            # engine serving this folder while the mount stands — adopted it.
            adopted = [
                line for line in app.driver.app_stderr_text().splitlines()
                if "third-party deposit adopted" in line and _log_path(NAME) in line
            ]
            assert adopted, (
                "no `third-party deposit adopted` line for the deposit on the agent's "
                f"stderr\n{seat.diagnostics()}"
            )
            listed = _run(seat, "ls", "-1", str(seat.path)).stdout.decode().split()
            assert [n for n in listed if not n.startswith(".")] == [NAME], (
                f"one adopted file, never a duplicate: {listed!r}"
            )

            # MUTATION (UI): and back. The mount comes down and the adopted
            # file is an ordinary file in the user's directory. Back to the
            # Folders page first: the mailbox step left the app on Mail.
            b.navigate_folders()
            scope = f"folder-row[{b.open_folder(folder)}]"
            assert loc.toggle_mode(scope=scope) == "always", (
                f"the switch did not move the row back to always; "
                f"error={app.error_text()!r}\n{seat.diagnostics()}"
            )
            _await_mount(seat, mounted=False)
            assert (seat.path / NAME).read_bytes() == MARKER
            assert not app.has_error(), f"the round trip surfaced an error: {app.error_text()!r}"
    finally:
        # The seat (and its agent) has stopped: a root still standing is a dead
        # daemon's, which wedges the rmtree and every later touch — bring it
        # down first. Never remove through a root that would not come down.
        if release_mount(tmp / "a" / "watch"):
            shutil.rmtree(tmp, ignore_errors=True)

    # ── At rest: sealed under the folder's scheme, nothing in clear. ──
    for path, raw in _everything_on_disk(nest):
        assert MARKER not in raw, (
            f"the adopted file's bytes rest in cleartext in {path} — adoption must "
            "re-seal under the folder's own scheme (file-sync.md § Third-party "
            "deposit ingress)"
        )
