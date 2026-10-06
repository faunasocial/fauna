"""tier_3: S2 did:plc identity mint end-to-end — real nest + real
``fauna-atproto-bridge`` + a fake PLC directory
(``docs/goal/behavior/atproto-pds-bridge.md`` § Identity + § State & data shape).

The full custody-split flow, driven the way a user's client will drive it:

1. a handled user enters the hosted level via the USER-class transition kind
   ``fauna.bridges.atproto.set_integration_level`` (the depth selector's one
   mutation path), supplying their client-generated **senior** rotation pubkey;
2. the admin approves the ``atproto.pds`` bridge (the S1 enrollment path);
3. the bridge reads the roster, provision-on-reads its sealed key blob,
   unseals it (FFI), builds + signs the PLC **genesis op**, and submits it to
   the (fake) PLC directory;
4. the submitted op proves the ratified key-custody split on the wire:
   ``rotationKeys[0]`` IS the user's key — senior to the bridge key;
5. the bridge reports back, nest stores the DID as data with provenance, and
   the primary domain's DNS matrix grows the ``_atproto.<handle>.<domain>``
   TXT row a managed-mode client would auto-publish.

The fake directory stands in for ``https://plc.directory`` via the test-only
``FAUNA_ATPROTO_PLC_DIRECTORY_URL`` seam (the ``FAUNA_BRIDGE_FAKE_DNS_JSON``
precedent — artifact/test IPC, never operator config).
"""

import os
import subprocess
import time

import pytest

from helpers.atproto_fakes import FakePlcDirectory

pytestmark = pytest.mark.tier_3

# The ATProto cryptography spec's p256 example vector — a syntactically valid
# senior rotation pubkey (the e2e never needs its secret half; a real client
# generates its own via fauna-client-atproto::rotation_key).
USER_ROTATION_PUB = "did:key:zDnaembgSGUhZULN2Caob4HLJPaxBh92N7rtH21TErzqf8HQo"  # gitleaks:allow
HANDLE_DOMAIN = "fauna.test"


@pytest.mark.feature("atproto")
def test_did_plc_mint_end_to_end(nest_binary, atproto_bridge_e2e_binary, tmp_path_factory):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_handled_actor
    from conftest import _make_nest, _repo_root
    from helpers.bridge_enrollment import approve_bridge
    from drivers.port_util import (
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
        untrack_process,
    )

    # The bridge binary is built by the `atproto_bridge_e2e_binary` fixture at
    # COLLECTION time — outside the per-test timeout budget; see
    # `_ensure_atproto_bridge_e2e_built`'s docstring in conftest.py for why an
    # in-body `just atproto-bridge-build` call used to die as a bare
    # `Timeout (>900.0s)` under fleet contention. The fixture also picks the
    # right binary per platform, so the windows/unix branch is gone from here.
    # The e2e FLAVOR, not the production binary: the FAUNA_ATPROTO_* seams this
    # test sets are compiled only into `-tags fauna_e2e_fixtures` (convention 15,
    # e2e-automation-surface-gating.md → the Go bridges' leg); the shipped
    # flavor ignores them, so against it this test could not even reach its
    # fakes. The shipped flavor itself is exercised by test_atproto_bridge_enroll
    # (tier_3) and the nest image tests (tier_4).
    atproto_bin = atproto_bridge_e2e_binary

    directory = FakePlcDirectory()
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "atproto-mint-nest",
        # The domained claim, which is also this nest's ONLY registration of
        # HANDLE_DOMAIN — the `add_local_domain` that used to sit further down
        # is gone, because two doors onto one domain means the loser's
        # arguments are silently discarded (`add_local_domain` is idempotent by
        # domain NAME). The claim's own cert mode is `expand_primary` where that
        # call asked for `per_host`, and the difference is inert here: nothing
        # in the DNS matrix or the `_atproto` TXT row reads
        # `mta_sts_cert_mode`, and a plain-HTTP test nest arms no ACME at all
        # (`testing.md` § Default app and nest mode, ruling (3)).
        claim_domain=HANDLE_DOMAIN,
    )
    from common.auth import open_registration
    open_registration(nest)
    proc = None
    log_fh = None
    try:
        admin = nest["admin"]

        # ── 1. A handled user enters the hosted level (the depth selector's
        # USER-class transition kind — the production enable path; the senior
        # rotation pubkey is the client-generated half of the ratified
        # custody split — its secret never leaves a real client). ──
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )
        alice_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=alice["actor_id_bytes"],
            signing_key=bytes(alice["signing_key"]),
        )
        with alice_ws:
            alice_ws.call(
                "fauna.bridges.atproto.set_integration_level",
                {
                    "target_level": "hosted_visible",
                    "did_method": "plc",
                    "user_rotation_pub_did_key": USER_ROTATION_PUB,
                    "history_backfill": False,
                },
            )

        # ── 2. Bridge cold boot against the fake directory + admin approval. ──
        tmp = tmp_path_factory.mktemp("atproto-mint-bridge")
        keyfile_path = tmp / "atproto.pds.key"
        minted = subprocess.run(
            [atproto_bin, f"--keypair-file={keyfile_path}", "--print-pubkey"],
            cwd=_repo_root, capture_output=True, text=True, check=True,
        )
        pubkey_hex = minted.stdout.strip()
        env = os.environ.copy()
        env["FAUNA_ATPROTO_PLC_DIRECTORY_URL"] = directory.url
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
            # The primary mail domain is already registered — the claim carried
            # it (`claim_domain=HANDLE_DOMAIN` above), which is what production
            # does and what this fixture used to reproduce by hand a few lines
            # later. The DNS matrix is per active mail domain, so the `_atproto`
            # TXT row in step 5 still finds its primary.
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                rows = admin_ws.call(
                    "fauna.bridges.list_service_users", {"status": "pending"}
                )["service_users"]
                if any(r["ed25519_pubkey"].hex() == pubkey_hex for r in map(
                    lambda r: {**r, "ed25519_pubkey": bytes(r["ed25519_pubkey"])}, rows
                )):
                    break
                assert proc.poll() is None, "bridge exited pre-enroll; log:\n" + _tail()
                time.sleep(0.5)
            approve_bridge(
                nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
            )

            # ── 3+4. The bridge mints: the fake directory receives the signed
            # genesis op. THE custody assertion: rotationKeys[0] is the USER's
            # key, the bridge's key strictly after it. ──
            deadline = time.monotonic() + 60
            submission = None
            while time.monotonic() < deadline:
                subs = directory.snapshot()
                if subs:
                    submission = subs[0]
                    break
                assert proc.poll() is None, (
                    "bridge exited before minting; log:\n" + _tail()
                )
                time.sleep(0.5)
            assert submission is not None, (
                "the bridge must submit a PLC genesis op; log:\n" + _tail()
            )
            did, op = submission
            assert did.startswith("did:plc:") and len(did) == len("did:plc:") + 24, did
            assert op["type"] == "plc_operation"
            assert op["prev"] is None
            assert op["sig"], "genesis op must be signed"
            rotation = op["rotationKeys"]
            assert rotation[0] == USER_ROTATION_PUB, (
                "the USER-custodied key must be SENIOR (rotationKeys[0]) — the "
                f"ratified custody split; got {rotation}"
            )
            assert len(rotation) == 2 and rotation[1].startswith("did:key:zQ3s"), (
                f"the bridge's junior K-256 rotation key must follow: {rotation}"
            )
            assert op["verificationMethods"]["atproto"].startswith("did:key:zQ3s")
            assert op["alsoKnownAs"] == [f"at://alice.{HANDLE_DOMAIN}"]
            # The PDS serves at the dedicated `pds.<domain>` subdomain — the
            # ratified F1 packaging resolution (`atproto-pds-full.md` § Wire &
            # process topology: the SNI router is pure L4, so XRPC needs its
            # own hostname), and the endpoint the op claims must be the one
            # the SNI router actually serves.
            assert (
                op["services"]["atproto_pds"]["endpoint"]
                == f"https://pds.{HANDLE_DOMAIN}"
            )
            assert op["services"]["atproto_pds"]["type"] == "AtprotoPersonalDataServer"

            # ── 5. Nest recorded the DID as data and the primary domain's
            # matrix carries the handle-verification TXT (`did=<did>`), which
            # a managed-mode client auto-publishes with no client change. ──
            deadline = time.monotonic() + 30
            txt_row = None
            while time.monotonic() < deadline:
                domains = admin_ws.call("fauna.dns.list_records", {})["domains"]
                primary = [d for d in domains if d["is_primary"]]
                if primary:
                    rows = [
                        r for r in primary[0]["records"]
                        if r["name"] == f"_atproto.alice.{HANDLE_DOMAIN}"
                    ]
                    if rows:
                        txt_row = rows[0]
                        break
                time.sleep(0.5)
            assert txt_row is not None, (
                "the _atproto handle TXT must appear on the primary matrix once "
                "the DID is recorded; log:\n" + _tail()
            )
            assert txt_row["record_type"] == "TXT"
            assert txt_row["expected"] == f'"did={did}"'
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
        directory.close()
        nest_cleanup()
