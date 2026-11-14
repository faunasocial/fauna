"""tier_3: the macOS File Provider extension is a relay seat of its own — a kept
body serves by relay, a placeholder answers none.

Owner: ``docs/goal/behavior/relay-serving.md`` § Relay serving → *Which seats
hold* → *Apple's File Provider replica*: on macOS the extension's host runs the
relay seat itself, over its own bearer connection, for its one set, while the
extension's process lives; it serves what its staging root holds, and a file
the OS evicted or never fetched answers none.

**What this pins that the tier below cannot.** One tier down,
``relay_serve_on_demand_test``'s
``a_staging_root_replica_serves_what_it_staged_and_declines_a_row_with_no_staged_copy``
drives the engine over a staging root by hand, and
``file_provider_host::tests::only_apples_host_on_macos_runs_its_own_seat``
pins that the host starts the seat. Neither shows that the real extension,
launched by the OS for a registered domain, announces its set on its own
connection and answers the nest's ask from its staging root.

**Why the domain comes from the signed bundle's test CLI.** The app's own
domain reconcile does nothing in an e2e launch (the SPM-built bare binary is
not the appex-embedding bundle, and the domain flip is e2e-gated off because a
File Provider domain is machine-global). The one live-domain driver is the
signed bundle's ``provision`` / ``register``, as in
``test_file_provider_read_path.py``, whose fixture and helpers this module
adopts. The iOS-only ``FAUNA_E2E_FILE_PROVIDER_RECONCILE`` lift registers no
macOS domain.

**How the staging root is read.** Through ``Fauna staged <set-ref>``, the test
CLI's read-only listing of the extension's staging root
(``fileProviderRootDir(for:)``), run in the signed app's own process. That
process may open the app-group container; a test process reading it would
raise the OS's other-app-data prompt on a console nobody watches.

**The journey.** One account, one metadata-only folder (its bytes never rest
on the nest, so a body moves between devices only by relay):

    seat a — the File Provider domain, driven through the signed bundle
    seat b — a tui seat, an always-resident engine under the account's agent

*Kept body serves.* A file is written into a's domain (``Fauna write``, a
coordinated write the OS hands the extension as ``createItem``). It lands in
a's staging root and is recorded; the nest's reachability verdict turns true
only once a's extension has announced the folder, since no other seat exists
yet. b starts afterwards and receives the bytes, which only a's extension
could have served.

*Placeholder declines.* b then writes a file of its own; a's domain lists it
(``Fauna signal``, the push nudge) but never fetches it. With b gone — the
folder's online destinations back to what they were before b started — a's
domain is asked to open that file. The only announced seat left is a's own,
whose staging root has no copy, so the open fails; and a's staging root still
holds no copy afterwards, while the kept body is still there.

Every wait is on a state, never on time (convention 14). Listing a's domain
directory while waiting keeps the OS handing the extension work, so its
process — and with it the seat — stays alive for the read (the OS ends an
idle extension; the ruling serves only while it lives).
"""

import contextlib
import secrets
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import blake3
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import register_user, set_folder_residency, sync_status, user_create_folder
from common.accounts import actor_id_hex
from common.auth import UNBINDING_MAX_DEVICES, _user_call, mint_and_hold_token, set_tier_caps
from helpers import sync_seats
from helpers.set_names import item_in_set
from helpers.waiting import wait_until
from tests.platform.macos.test_file_provider_read_path import (  # noqa: F401 — fauna_fp_host_app is a pytest fixture adopted by import
    _BACKUP_KEY_CONTEXT,
    _CLOUD,
    _has_codesigning_identity,
    _run_host,
    _safe_listdir,
    fauna_fp_host_app,
)

pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="macOS-only: File Provider extension"),
    pytest.mark.skipif(
        not _has_codesigning_identity(),
        reason="appex-driven FP tier_3 is signing-gated: the app-group data-protection "
        "Keychain rendezvous returns errSecMissingEntitlement (-34018) under ad-hoc signing. "
        "The seat is pinned headlessly by relay_serve_on_demand_test.",
    ),
    pytest.mark.tier_3,
    pytest.mark.macos,
]

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
WINDOW = 180.0
# `Fauna read` blocks in the OS's fetchContents until the extension answers;
# the relay settles a decline at once, so this only bounds a wedged OS.
READ_CEILING = 300

_KEPT = "kept.txt"
_PEER = "peer.txt"


def _staged(fauna: str, folder_ref: str) -> dict[str, str]:
    """What a's staging root holds: ``{rel: sha256}`` from ``Fauna staged``."""
    proc = _run_host(fauna, "staged", folder_ref)
    out: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        if line.startswith("staged: ") and " sha256=" in line:
            rel = line[len("staged: "):].split(" bytes=", 1)[0]
            out[rel] = line.rsplit(" sha256=", 1)[1].strip()
    return out


def _sha256(data: bytes) -> str:
    import hashlib

    return hashlib.sha256(data).hexdigest()


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 6 * WINDOW + READ_CEILING + 120))
@pytest.mark.feature("device-to-device-file-transfer")
def test_the_file_provider_extension_serves_its_kept_body_and_declines_a_placeholder(
    nest_instance, request, fauna_fp_host_app
):
    from tests.test_filesync_seats import handle_for, harness_sign_in

    fauna = fauna_fp_host_app
    port, url = nest_instance["port"], nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # ── Fixture setup: one account, one metadata-only folder. ──
    secret_key = secrets.token_hex(32)
    actor_id = actor_id_hex(secret_key)
    handle = handle_for(actor_id)
    register_user(port, actor_id, base_url=url, admin_signing_key=admin_sk, handle=handle)
    set_tier_caps(port, admin_signing_key=admin_sk, max_devices=UNBINDING_MAX_DEVICES,
                  base_url=url)
    seed = bytes.fromhex(secret_key)
    folder = f"fp-relay-{secrets.token_hex(4)}"
    created = user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    folder_ref = f"local:{created['id']}"
    # Metadata-only BEFORE any engine first sees the folder, so no seat ever
    # uploads these bytes and the relay is the only way they can move.
    reply = set_folder_residency(
        port, folder, secret_key=secret_key, residency="metadata_only", base_url=url
    )
    assert reply.get("ok") is True, f"residency set refused: {reply!r}"

    # The extension's device and bearer (the read-path test's provision shape).
    from nacl.signing import SigningKey

    sk = SigningKey(seed)
    token = mint_and_hold_token(url, sk)
    ext_device = secrets.token_bytes(32)
    with WsRpcAdminClient(url, actor_id=bytes(sk.verify_key), signing_key=seed) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": ext_device.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
    backup_key = blake3.blake3(seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()

    kept_body = f"kept on the extension alone {secrets.token_hex(16)}\n"
    peer_body = f"written by seat b, never fetched by a {secrets.token_hex(16)}\n".encode()

    run_token = sync_seats.new_run_token(seats=2)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))

    def seat_b():
        return sync_seats.make_seat(
            "tui",
            name="b",
            run_token=run_token,
            root=tmp / "b",
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
        return [item for item in items if item_in_set(item, folder)]

    def status() -> dict:
        return sync_status(port, secret_key=secret_key, folder=folder, base_url=url)

    def online_destinations() -> int:
        return sum(1 for d in status().get("destinations", []) if d.get("online"))

    domain_dir_box: list[Path] = []

    def touch_domain() -> None:
        # Keep the OS handing the extension work (module docstring).
        if domain_dir_box:
            _safe_listdir(domain_dir_box[0])

    def a_diag() -> str:
        try:
            staged = _staged(fauna, folder_ref)
        except AssertionError as e:
            staged = {"<staged failed>": str(e)}
        listing = _safe_listdir(domain_dir_box[0]) if domain_dir_box else []
        return f"--- seat a (extension) ---\nstaged={staged!r}\ndomain listing={listing!r}"

    before = set(_safe_listdir(_CLOUD)) if _CLOUD.exists() else set()
    try:
        _run_host(
            fauna, "provision", url, actor_id, ext_device.hex(), "fp-ext",
            backup_key.hex(), token,
        )
        _run_host(fauna, "register", folder_ref, folder)
        _CLOUD.mkdir(parents=True, exist_ok=True)
        domain_dir_box.append(wait_until(
            lambda: next((_CLOUD / d for d in set(_safe_listdir(_CLOUD)) - before), None),
            WINDOW, interval=1.0,
            diagnose=lambda: f"no domain directory appeared under {_CLOUD} after register",
        ))

        # ── KEPT BODY SERVES. ──
        _run_host(fauna, "write", folder_ref, folder, _KEPT, kept_body)
        wait_until(
            lambda: len(listed()) == 1, WINDOW, interval=1.0,
            diagnose=lambda: f"the extension never recorded {_KEPT!r}\n{a_diag()}",
        )
        staged = _staged(fauna, folder_ref)
        assert staged.get(_KEPT) == _sha256(kept_body.encode()), (
            f"{_KEPT!r} is recorded but not held byte for byte in a's staging root — "
            f"the body the seat would serve\n{a_diag()}"
        )
        # The causal barrier for b's read: no other seat exists, so the verdict
        # is true only once a's extension has announced the folder.
        wait_until(
            lambda: (touch_domain(), status().get("source_online") is True)[1],
            WINDOW, interval=1.0,
            diagnose=lambda: (
                "a's extension never announced the folder on its own connection, so the "
                f"nest has no seat to ask for its bytes\nstatus={status()!r}\n{a_diag()}"
            ),
        )
        online_before_b = online_destinations()

        with contextlib.ExitStack() as stack:
            b = stack.enter_context(seat_b())
            b.await_ready(STARTUP_WINDOW)
            kept_on_b = b.path / _KEPT

            def landed() -> bool:
                touch_domain()
                try:
                    return kept_on_b.read_bytes() == kept_body.encode()
                except OSError:
                    return False

            wait_until(
                landed, WINDOW, interval=1.0,
                diagnose=lambda: (
                    f"{_KEPT!r} never reached seat b byte for byte — the folder is "
                    "metadata-only, so the bytes can only come from a's extension by relay\n"
                    f"--- seat b ---\n{b.diagnostics()}\n{a_diag()}"
                ),
            )

            # ── PLACEHOLDER DECLINES: b writes a file a only lists. ──
            (b.path / _PEER).write_bytes(peer_body)
            wait_until(
                lambda: len(listed()) == 2, WINDOW, interval=1.0,
                diagnose=lambda: f"seat b never recorded {_PEER!r}\n{b.diagnostics()}",
            )
            _run_host(fauna, "signal", folder_ref, folder)
            wait_until(
                lambda: _PEER in _safe_listdir(domain_dir_box[0]), WINDOW, interval=1.0,
                diagnose=lambda: f"a's domain never listed {_PEER!r} after the nudge\n{a_diag()}",
            )
            assert _PEER not in _staged(fauna, folder_ref), (
                f"a's staging root holds {_PEER!r} before anything opened it\n{a_diag()}"
            )

        # The writer is gone: its connection, and with it its announce, closes.
        wait_until(
            lambda: online_destinations() <= online_before_b, WINDOW, interval=1.0,
            diagnose=lambda: f"seat b's device still reads online after it stopped\nstatus={status()!r}",
        )

        read = subprocess.run(
            [fauna, "read", folder_ref, folder, _PEER],
            capture_output=True, text=True, timeout=READ_CEILING,
        )
        assert read.returncode != 0, (
            f"opening {_PEER!r} on a succeeded with its writer gone — in a metadata-only "
            "folder no seat but b ever held it, so a body came from somewhere it must not: "
            f"{read.stdout.strip()} {read.stderr.strip()}\n{a_diag()}"
        )
        staged = _staged(fauna, folder_ref)
        assert _PEER not in staged, (
            f"a's staging root holds {_PEER!r} after the failed open — a placeholder must "
            f"answer none, never be fetched into the staging root\n{a_diag()}"
        )
        assert staged.get(_KEPT) == _sha256(kept_body.encode()), (
            f"a's kept body left its staging root\n{a_diag()}"
        )
    finally:
        _run_host(fauna, "remove", folder_ref, folder, check=False)
        _run_host(fauna, "revoke", check=False)
        shutil.rmtree(tmp, ignore_errors=True)
