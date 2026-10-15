"""The background sync helper never holds your identity key.

Owner: ``docs/goal/architecture/apps/sync-agent-credentials.md`` § Credential
model — the agent is a bearer-only + minted-capability host (never the identity
seed, ``key-material-hierarchy.md`` rules #6/#7).

One tui seat, no operator (convention 16). Under the e2e file backend the app
and the helper it spawns share one credential namespace file in the launch's
credential dir (``FAUNA_KEYRING_APP`` is inherited), told apart by account key:
the helper persists its capability under ``capability/v1``
(``fauna_ipc::sync::CAPABILITY_KEY``), the app keeps the identity secret under
its own account keys. The witness reads what the helper PERSISTED — the
``capability/v1`` value, in every encoding the harness could have written it —
and searches it for the identity secret, with the other rows of the same
credential dir as the positive control (the search finds the secret where it is
known to rest, so its absence from the capability means something). Only
booleans and key names reach a failure message — never values.
"""

import base64
import binascii
import contextlib
import json
import secrets
import shutil
import sys
import tempfile
from pathlib import Path

import pytest

from common import user_create_folder
from helpers import sync_seats
from helpers.waiting import wait_until
from tests.test_device_to_device_file_transfer import (
    STARTUP_WINDOW,
    WINDOW,
    _account,
    _seat_factory,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
]

_SEAT_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in sync_seats.seat_sets_for_platform(sys.platform, (("tui", "tui"),))
]

#: ``fauna_ipc::sync::CAPABILITY_KEY`` — the helper's persisted record.
CAPABILITY_KEY = "capability/v1"


def _decodings(value: str) -> list[bytes]:
    """Every byte string a stored value could stand for: itself as text, and
    its hex / base64 / urlsafe-base64 decodings where they parse."""
    out = [value.encode("utf-8", errors="replace")]
    try:
        out.append(bytes.fromhex(value))
    except ValueError:
        pass
    for decode in (base64.b64decode, base64.urlsafe_b64decode):
        try:
            out.append(decode(value + "=" * (-len(value) % 4)))
        except (binascii.Error, ValueError):
            pass
    return out


def _holds(value, secret_key_hex: str) -> bool:
    """Whether ``value`` carries the identity secret, raw or in a text form."""
    raw = bytes.fromhex(secret_key_hex)
    text = value if isinstance(value, str) else json.dumps(value, separators=(",", ":"))
    forms = [raw, secret_key_hex.lower().encode()]
    return any(any(form in blob for form in forms) for blob in _decodings(text))


@pytest.mark.timeout(int(STARTUP_WINDOW + WINDOW + 120))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("local-folder-sync")
def test_the_sync_helper_never_holds_the_identity_key(nest_instance, request, seat_pair):
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"nokey-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    run_token = sync_seats.new_run_token(seats=1)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            a.await_ready(STARTUP_WINDOW)
            cred_dir = Path(a.app.driver._resolved_credential_dir)

            def rows() -> dict:
                found: dict = {}
                for path in sorted(cred_dir.glob("*.json")):
                    try:
                        data = json.loads(path.read_text(encoding="utf-8"))
                    except (OSError, ValueError):
                        continue
                    if isinstance(data, dict):
                        found.update({(path.name, k): v for k, v in data.items()})
                return found

            def capability():
                return [v for (_, k), v in rows().items() if k == CAPABILITY_KEY]

            wait_until(
                lambda: bool(capability()), WINDOW, interval=1.0,
                diagnose=lambda: (
                    f"the sync helper never persisted a {CAPABILITY_KEY!r} record under the "
                    f"seat's credential dir; rows: {sorted(rows())!r}"
                ),
            )
            others = {k: v for k, v in rows().items() if k[1] != CAPABILITY_KEY}
            assert any(_holds(v, secret_key) for v in others.values()), (
                f"positive control failed: the identity secret rests in none of the other "
                f"credential rows {sorted(others)!r}, so a search of the helper's "
                f"capability proves nothing"
            )
            assert not any(_holds(v, secret_key) for v in capability()), (
                f"the sync helper's persisted {CAPABILITY_KEY!r} record carries the "
                f"identity secret"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
