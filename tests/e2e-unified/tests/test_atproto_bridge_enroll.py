"""tier_3: the ATProto PDS bridge (role ``atproto.pds``) enrolls end-to-end
against a real nest — the S1 cold-boot handshake it inherits VERBATIM from the
mail bridge (``docs/goal/behavior/mail-bridge-lifecycle.md`` § Cold boot), under
the new ``atproto.pds`` role (``docs/goal/behavior/atproto-pds-bridge.md``
§ Bridge lifecycle).

Zero-touch self-enrollment, the real S1 flow: the ``fauna-atproto-bridge``
process cold-boots on a fresh keypair, self-announces via
``fauna.bridges.request_enrollment`` with ``role_hint="atproto.pds"`` → a
**PENDING** row (this role is **NEVER auto-approved** — mail-bridge-lifecycle.md
§ Onboarding auto-approval, Scope: "A future non-mail bridge role (ATProto,
Nostr, …) is not auto-approved … falls back to the manual approval card"). An
admin approves it, and the bridge then authenticates, ``whoami``'s role
``atproto.pds``, attests its x25519 via ``register_service_user``,
``fetch_config``'s, and **idles** (there is no PDS surface yet — repo/MST/DID/
firehose are S2/S3). This pins the process + its nest handshake end-to-end across
both binaries.

The nest-side role acceptance (``BridgeRole::AtprotoPds`` through the whole
``fauna.bridges.*`` flow, the data-preserving CHECK-widen migration, and the
least-privilege ``CallerClass::BridgeAtprotoPds``) is covered by Rust unit tests
in ``bins/fauna-nest``; the Go boot sequence by ``cmd/fauna-atproto-bridge``
unit tests. THIS is the full cross-binary wire proof.
"""

import os
import subprocess
import time

import pytest

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("atproto")
def test_atproto_bridge_enrolls_end_to_end(nest_binary, atproto_bridge_binary, tmp_path_factory):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _make_nest, _repo_root
    from helpers.bridge_enrollment import approve_bridge
    from drivers.port_util import (
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
        untrack_process,
    )

    # The bridge binary is built by the `atproto_bridge_binary` fixture at
    # COLLECTION time — outside the per-test timeout budget; see
    # `_ensure_atproto_bridge_built`'s docstring in conftest.py for why an
    # in-body `just atproto-bridge-build` call used to die as a bare
    # `Timeout (>900.0s)` under fleet contention. The fixture also picks the
    # right binary per platform, so the windows/unix branch is gone from here.
    atproto_bin = atproto_bridge_binary

    nest, nest_cleanup = _make_nest(nest_binary, tmp_path_factory, "atproto-enroll-nest")
    proc = None
    log_fh = None
    try:
        admin = nest["admin"]
        tmp = tmp_path_factory.mktemp("atproto-bridge")
        keyfile_path = tmp / "atproto.pds.key"

        # Mint the keypair + capture its pubkey via --print-pubkey (the artifact's
        # idempotent mint-if-absent provisioning step). We need the pubkey hex to
        # approve the pending enrollment; the same keyfile is loaded by the spawn.
        minted = subprocess.run(
            [atproto_bin, f"--keypair-file={keyfile_path}", "--print-pubkey"],
            cwd=_repo_root, capture_output=True, text=True, check=True,
        )
        pubkey_hex = minted.stdout.strip()
        assert len(pubkey_hex) == 64, f"unexpected --print-pubkey output {pubkey_hex!r}"
        pub_bytes = bytes.fromhex(pubkey_hex)

        # ── Cold boot: the bridge self-enrolls (request_enrollment, role_hint
        # atproto.pds → a PENDING row) and polls for approval on the anonymous WS. ──
        env = os.environ.copy()
        log_path = tmp / "bridge.log"
        log_fh = open(log_path, "wb")
        proc = subprocess.Popen(
            [
                atproto_bin,
                f"--keypair-file={keyfile_path}",
                f"--nest-endpoint={nest['url']}",
                f"--data-dir={tmp.as_posix()}",
                "--log-level=debug",
            ],
            stdout=log_fh, stderr=subprocess.STDOUT, env=env,
            **popen_group_kwargs(),
        )
        # Windows half of the die-with-the-run guarantee — `popen_group_kwargs()`
        # is `{}` there, so without this the bridge's only protection is the
        # atexit sweep a killed run never reaches (testing.md § point 9). No-op
        # off Windows.
        reap_descendants_of(proc.pid)
        track_process(proc)

        def _tail() -> str:
            return log_path.read_text(errors="replace")[-4000:]

        admin_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            # ── 1. The bridge lands PENDING with role atproto.pds. The admin
            # roster (holder_view=false) shows the full set including atproto.pds;
            # a mail user's holder view deliberately would NOT. ──
            deadline = time.monotonic() + 30
            pending_row = None
            while time.monotonic() < deadline:
                rows = admin_ws.call(
                    "fauna.bridges.list_service_users", {"status": "pending"}
                )["service_users"]
                match = [r for r in rows if bytes(r["ed25519_pubkey"]) == pub_bytes]
                if match:
                    pending_row = match[0]
                    break
                assert proc.poll() is None, (
                    "bridge exited before enrolling; log:\n" + _tail()
                )
                time.sleep(0.5)
            assert pending_row is not None, (
                "the atproto.pds bridge must appear PENDING (never auto-approved); "
                "log:\n" + _tail()
            )
            assert pending_row["role"] == "atproto.pds", (
                f"pending role = {pending_row['role']!r}, want atproto.pds"
            )

            # ── 2. Admin approves the pending bridge by its pubkey. This is the
            # programmatic equivalent of clicking `admin-bridges-pending-approve-
            # button` (the role-agnostic pending-approval card, exercised through
            # the UI by helpers/caldav_onboarding.py::_approve_pending_bridge); the
            # S4 atproto enable UX layers on top of this same approval path. ──
            approve_bridge(
                nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
            )

            # ── 3. The bridge authenticates, whoami's role atproto.pds, attests
            # x25519 via register_service_user, and fetch_config's: the approved
            # roster row shows role atproto.pds WITH x25519 attested (has_x25519).
            # has_x25519 flipping true is the nest-side proof register_service_user
            # ran → the bridge completed the authed handshake, not just approval. ──
            deadline = time.monotonic() + 30
            approved_row = None
            while time.monotonic() < deadline:
                rows = admin_ws.call(
                    "fauna.bridges.list_service_users", {"status": "approved"}
                )["service_users"]
                match = [r for r in rows if bytes(r["ed25519_pubkey"]) == pub_bytes]
                if match and match[0].get("has_x25519"):
                    approved_row = match[0]
                    break
                assert proc.poll() is None, (
                    "bridge exited before completing the approved handshake; log:\n"
                    + _tail()
                )
                time.sleep(0.5)
            assert approved_row is not None, (
                "the approved atproto.pds row with an attested x25519 must appear "
                "(whoami + register_service_user completed); log:\n" + _tail()
            )
            assert approved_row["role"] == "atproto.pds"
            assert approved_row["has_x25519"] is True

        # ── 4. fetch_config returned + the bridge reached its idle state. S1 opens
        # no listeners, so the bridge's own structured log is the observable for
        # these last two cold-boot steps (its stdout IS the process interface). ──
        deadline = time.monotonic() + 10
        log_text = ""
        while time.monotonic() < deadline:
            log_text = log_path.read_text(errors="replace")
            if "config snapshot fetched" in log_text and "enrolled;" in log_text:
                break
            time.sleep(0.5)
        assert "config snapshot fetched" in log_text, (
            "fetch_config must return; log:\n" + log_text[-4000:]
        )
        # S2 replaced the S1 idle block with the mint poll loop; the
        # post-enroll steady-state line is now "enrolled; mint loop starting".
        assert "enrolled;" in log_text, (
            "the bridge must reach its post-enroll steady state; log:\n"
            + log_text[-4000:]
        )
        # With no pending identities the mint loop is quiescent — the process
        # must NOT crash after enrolling (a crash here would read downstream
        # as a lost enrollment).
        assert proc.poll() is None, "the bridge must stay up after enrolling, not exit"
    finally:
        if proc is not None:
            try:
                if proc.poll() is None:
                    proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()
            finally:
                untrack_process(proc)
        if log_fh is not None:
            log_fh.close()
        nest_cleanup()
