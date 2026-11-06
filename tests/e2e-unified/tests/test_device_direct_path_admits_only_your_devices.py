"""Only your own devices can fetch from your device over the direct path, and
what they fetch is encrypted on the way.

Owner: ``docs/goal/behavior/p2p.md`` § Wormability posture — rule 1, *admission
before parsing* (everything past ``node_info`` is checked against the device's
``DeviceAuthorization`` before any data-plane parsing), and rule 4, *received
content is inert* (peers exchange hash-addressed AEAD ciphertext) — under
§ Goal's promise of an encrypted direct tunnel.

**Why a probe.** No app gesture makes a device of another account dial yours:
the peer leg dials only the account's own fleet, whose dial candidates travel
over a fleet-only kind. So the stranger's side is the agent's test-only probe
arm (``ProbePeerLeg``, ``fauna-sync-agent``'s ``peer_probe``; compiled out of
release artifacts, convention 15) — the request a hostile device would send:
it dials as its own device, runs the admission exchange with its own
enrollment witness, then asks for the chunk bodies **whatever the admission
said**, and sends one malformed chunk request whose answer says which ran
first on the serving side, the admission check or the parser. The shape is the
share plane's stranger probe (``test_share_pump_two_actor.py``), on the
same-account leg.

**The probe is its own control.** Run first from seat ``b`` — the same
account as seat ``a`` — against ``a`` and the same file, it must be admitted,
served the file's chunks, and its malformed request must meet the parser. Only
the identity differs between that run and the stranger's, so the stranger's
empty report cannot be a probe that never worked. The stranger is handed the
very store keys the control was served.

**What each outcome rests on.**

- *Only your devices* (rule 1): the stranger dialed ``a``'s listener, ``a``
  refused its witness at the admission exchange, ``a`` answered its chunk pull
  with the admission refusal and no body crossed, and ``a`` answered its
  malformed request with the admission refusal too — never the parser's — so
  the refusal came before any parsing. Every one of those codes is ``a``'s own
  answer on the wire.
- *Encrypted on the way* (rule 4, and § Goal's tunnel): what crossed the wire
  to the admitted control is the stored chunk sealed under the folder's key —
  every body is checked against its hash address by the production pull, and
  none of it contains any 32-byte run of the file's plaintext — over a channel
  whose handshake proved the far end holds ``a``'s node key. That observes the
  payload layer, which is what the outcome promises a bystander cannot read;
  the transport's own TLS 1.3 record layer beneath it is the QUIC stack's, not
  this test's, to pin.

Three seats on one machine, no operator (convention 16): ``a`` and ``b`` of one
account, ``c`` of a second account on the same nest, each the platform's pair
mode. Every wait is a deadline poll on state (convention 14).
"""

import contextlib
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
    SIBLING_WINDOW,
    STARTUP_WINDOW,
    WINDOW,
    _account,
    _await_bytes,
    _await_siblings,
    seat_pairs,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; linux, windows and macos run their native pairs through the
    # same seat seam — the probe arm lives in the one sync agent every desktop
    # app spawns. android and iOS run no seat pair (their absence of both
    # outcomes is declared on the catalog page).
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
]

_SEAT_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in seat_pairs(sys.platform)
]

# Under one chunk, so the file crosses as one sealed body.
BODY_BYTES = 64 * 1024 + 7
# A plaintext run this long found in a served body would mean the body is not
# ciphertext.
WINDOW_BYTES = 32

NOT_ADMITTED = "fauna.peer.sync.not_admitted"
WITNESS_REFUSED = "fauna.peer.sync.witness_refused"
MALFORMED = "fauna.protocol.malformed"


def _seat(nest_instance, request, account, folder, root, run_token, mode, name):
    from tests.test_filesync_seats import harness_sign_in

    port, url, secret_key, actor_id, handle = account
    return sync_seats.make_seat(
        mode,
        name=name,
        run_token=run_token,
        root=root / name,
        node_url=url,
        node_port=port,
        folder=folder,
        sign_in=harness_sign_in(url, secret_key, actor_id, handle),
        request=request,
    )


def _dial_target(seat) -> dict:
    """``seat``'s own peer-leg dial target, once its leg is bound."""
    found: dict = {}
    last: list = []

    def bound() -> bool:
        client = seat.agent_client()
        if client is None:
            return False
        try:
            found.update(client.peer_leg_dial_target())
            return bool(found.get("lan_endpoints"))
        except Exception as e:  # not bound yet: the agent says so
            last[:] = [repr(e)]
            return False

    wait_until(
        bound, SIBLING_WINDOW, interval=2.0,
        diagnose=lambda: (
            f"seat {seat.name}'s agent never bound a peer leg with a LAN candidate "
            f"(last answer {last!r})\n--- seat {seat.name} ---\n{seat.diagnostics()}"
        ),
    )
    return dict(found)


def _folder_id(seat, folder: str) -> str:
    """The folder's ``FolderRef`` wire string on ``seat``'s agent."""
    for row in seat.agent_client().share_serve_info():
        if row.get("folder") == folder:
            return row["folder_id"]
    raise AssertionError(
        f"seat {seat.name}'s agent binds no folder {folder!r}\n{seat.diagnostics()}"
    )


def _plaintext_runs(body: bytes) -> set:
    return {
        body[i:i + WINDOW_BYTES].hex()
        for i in range(0, len(body) - WINDOW_BYTES + 1, WINDOW_BYTES)
    }


@pytest.mark.timeout(int(3 * STARTUP_WINDOW + 3 * SIBLING_WINDOW + WINDOW + 180))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("device-to-device-file-transfer")
def test_a_device_that_is_not_yours_fetches_nothing_and_yours_fetch_only_ciphertext(
    nest_instance, request, seat_pair
):
    """Seats ``a`` and ``b`` of one account hold a file; ``b``'s probe of ``a``
    is admitted and served sealed bodies only. Seat ``c``, of another account
    on the same nest, probes ``a`` for the same keys and is refused at
    admission, before any parsing, with nothing served."""
    own = _account(nest_instance)
    other = _account(nest_instance)
    folder = f"direct-only-{secrets.token_hex(4)}"
    user_create_folder(own[0], folder, secret_key=own[2], base_url=own[1])
    stranger_folder = f"stranger-{secrets.token_hex(4)}"
    user_create_folder(other[0], stranger_folder, secret_key=other[2], base_url=other[1])

    run_token = sync_seats.new_run_token(seats=3)
    name = f"{run_token}-direct.bin"
    body = secrets.token_bytes(BODY_BYTES)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    names = sync_seats.seat_names(3)
    mode = seat_pair[0]

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(
                _seat(nest_instance, request, own, folder, tmp, run_token, seat_pair[0], names[0])
            )
            b = stack.enter_context(
                _seat(nest_instance, request, own, folder, tmp, run_token, seat_pair[1], names[1])
            )
            c = stack.enter_context(
                _seat(nest_instance, request, other, stranger_folder, tmp, run_token, mode,
                      names[2])
            )
            for seat in (a, b, c):
                seat.await_ready(STARTUP_WINDOW)
            # The direct path exists between a and b, so a's leg is bound and
            # its serve door is the one under test.
            _await_siblings(b, a, "before the probes")

            (a.path / name).write_bytes(body)
            _await_bytes(b, name, body, "the probed file", a)

            target = _dial_target(a)
            folder_id = _folder_id(a, folder)

            # ── the control: a device of a's own account ──
            control = b.agent_client(timeout=60.0).probe_peer_leg(
                target, folder_id, relative_path=name
            )
            where = (
                f"\n--- seat a ---\n{a.diagnostics()}\n--- seat b ---\n{b.diagnostics()}"
            )
            assert control.get("dialed") and control.get("admitted"), (
                f"[control] seat b, of a's own account, was not admitted by a — the "
                f"probe proves nothing until it is: {control!r}{where}"
            )
            assert control.get("peer_identity") == target["node_id"], (
                f"[control] the channel's handshake proved a different key than a's "
                f"node id {target['node_id']}: {control!r}"
            )
            assert control.get("store_keys") and control.get("bodies"), (
                f"[control] a served b none of the file's chunks: {control!r}{where}"
            )
            assert not control.get("pull_error") and not control.get("missing"), (
                f"[control] a did not serve every chunk of the file: {control!r}{where}"
            )
            assert control.get("malformed_error") == MALFORMED, (
                f"[control] an admitted device's malformed request must meet a's "
                f"parser: {control!r}"
            )

            # ── outcome 4: what crossed is ciphertext ──
            runs = _plaintext_runs(body)
            for served in control["bodies"]:
                wire = served["bytes"]
                leaked = [r for r in runs if r in wire]
                assert not leaked, (
                    f"[control] the body a served for {served['store_key']} carries "
                    f"{len(leaked)} {WINDOW_BYTES}-byte run(s) of the file's plaintext — "
                    f"it crossed the direct path unsealed"
                )

            # ── outcome 17: a device that is not yours ──
            _dial_target(c)  # c's own leg is bound, so its probe dials as itself
            stranger = c.agent_client(timeout=60.0).probe_peer_leg(
                target, folder_id, store_keys=control["store_keys"]
            )
            where += f"\n--- seat c ---\n{c.diagnostics()}"
            assert stranger.get("dialed"), (
                f"[stranger] the probe never reached a's listener, so it proves "
                f"nothing about a's door: {stranger!r}{where}"
            )
            assert not stranger.get("admitted") and stranger.get("admit_error") == WITNESS_REFUSED, (
                f"[stranger] a must refuse another account's device at the admission "
                f"exchange: {stranger!r}{where}"
            )
            assert not stranger.get("bodies") and stranger.get("pull_error") == NOT_ADMITTED, (
                f"[stranger] a served chunk bodies to a device that is not the account's, "
                f"or refused it for another reason than admission: {stranger!r}{where}"
            )
            assert stranger.get("malformed_error") == NOT_ADMITTED, (
                f"[stranger] a parsed an unadmitted device's request before refusing it "
                f"(wormability rule 1, admission before parsing): {stranger!r}{where}"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
