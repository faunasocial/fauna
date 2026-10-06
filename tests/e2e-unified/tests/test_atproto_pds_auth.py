"""tier_3: the ATProto PDS bridge F1 auth core (+ F3 phase 4 preferences) end-to-end.

While a session is live, the same authed client also round-trips
``app.bsky.actor.{put,get}Preferences`` (F3 phase 4 success,
``atproto-pds-full.md`` § F3 detail): a locally-served, never-proxied write/read
against real nest state, byte-for-byte opaque passthrough — reusing the live
session here rather than duplicating the whole three-process setup.

F1's definition of success (``docs/goal/behavior/atproto-pds-full.md`` § F1
detail): a raw XRPC client authenticates with a handle + app credential against
the ``fauna-atproto-bridge`` and calls the authed no-op ``getSession``; the
mint/list/revoke flows exist end-to-end at the shared-Rust + nest level, a
revocation kills the derived session, and the per-account external-apps
kill-switch refuses the whole plane when OFF. The final phase proves the
sealed HS256 secret source (the F1 remainder): a bridge restart keeps
pre-restart access tokens valid, because the secret is nest-minted on first
fetch, sealed to the bridge's attested x25519, and re-fetched at every boot.

What this exercises that the unit + handler tests can't: the REAL wire between
three processes — a raw HTTP XRPC client → the Go bridge's ``--xrpc-listen``
listener (createSession: identifier resolve → verifier fetch over WS-RPC →
Argon2id verify bridge-side → record_session → HS256 mint) → the nest's
``fauna.bridges.atproto.*`` registry. Only tier_3 catches a wire-shape or
cross-process auth drift between the Go verifier and the Rust minter, or a
CBOR-tag mismatch on the new kinds.

Credential provisioning is arranged over the nest's User-class
``provision_app_credential`` kind (e2e taxonomy rule 8b: fixture setup arranging
the precondition, not the behavior under test — the behavior under test is the
external-protocol XRPC login, which by construction has no Fauna app UI). The
client-UI mint surface is the deferred F1 client-surface half; when it lands, a
client-UI-driven mint test should follow (rule 8 carve-out).
"""

import os
import subprocess
import time

import pytest

from helpers.atproto_fakes import FakePlcDirectory
from helpers.xrpc_client import (
    load_app_credential_fixture as _load_fixture,
    xrpc_get as _xrpc_get,
    xrpc_post as _xrpc_post,
)

pytestmark = pytest.mark.tier_3

# The USER-custodied senior rotation pubkey the hosted-level transition
# requires (the ratified key-custody split — `test_atproto_identity_mint.py`
# proves the op-shape half; here it is only setup).
USER_ROTATION_PUB = "did:key:zDnaembgSGUhZULN2Caob4HLJPaxBh92N7rtH21TErzqf8HQo"  # gitleaks:allow


@pytest.mark.feature("atproto")
def test_atproto_pds_f1_auth_core_end_to_end(nest_binary, atproto_bridge_e2e_binary, tmp_path_factory):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_handled_actor
    from conftest import _make_nest, _repo_root
    from helpers.bridge_enrollment import approve_bridge
    from drivers.port_util import (
        find_free_port,
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
        untrack_process,
    )

    secret, verifier = _load_fixture()

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

    # A claimed nest whose handle domain lets us seed a handled actor "alice"
    # over the wire (the posture is opened post-claim);
    # resolve_handle("alice") is what
    # createSession maps the login identifier through.
    handle_domain = "fauna.test"
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "atproto-auth-nest",
        claim_domain=handle_domain,    )
    from common.auth import open_registration
    open_registration(nest)
    proc = None
    directory = None
    log_fh = None
    try:
        admin = nest["admin"]
        tmp = tmp_path_factory.mktemp("atproto-auth-bridge")
        keyfile_path = tmp / "atproto.pds.key"
        xrpc_port = find_free_port()
        xrpc_base = f"https://127.0.0.1:{xrpc_port}"

        minted = subprocess.run(
            [atproto_bin, f"--keypair-file={keyfile_path}", "--print-pubkey"],
            cwd=_repo_root, capture_output=True, text=True, check=True,
        )
        pubkey_hex = minted.stdout.strip()
        pub_bytes = bytes.fromhex(pubkey_hex)

        # The mint loop needs a PLC directory to submit alice's genesis op to —
        # since slice 4d createSession requires an ACTIVE hosted identity, so
        # this F1 auth test now needs the S2 mint round-trip in its setup.
        directory = FakePlcDirectory()
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
                f"--xrpc-listen=127.0.0.1:{xrpc_port}",
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

        # ── Approve the bridge (same path as the enroll test). ──
        admin_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                rows = admin_ws.call(
                    "fauna.bridges.list_service_users", {"status": "pending"}
                )["service_users"]
                if any(bytes(r["ed25519_pubkey"]) == pub_bytes for r in rows):
                    break
                assert proc.poll() is None, "bridge exited before enrolling:\n" + _tail()
                time.sleep(0.5)
            else:
                pytest.fail("atproto.pds bridge never appeared PENDING:\n" + _tail())

        approve_bridge(
            nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
        )

        # ── The bridge completes the authed handshake and brings up the XRPC
        # listener. Its structured log is the observable (the port opening is
        # the F1 surface coming alive). ──
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if "xrpc listener up" in log_path.read_text(errors="replace"):
                break
            assert proc.poll() is None, "bridge exited before serving XRPC:\n" + _tail()
            time.sleep(0.5)
        else:
            pytest.fail("XRPC listener never came up:\n" + _tail())

        # describeServer is public + static — proves the listener routes before
        # we depend on auth.
        status, body = _xrpc_get(xrpc_base, "com.atproto.server.describeServer")
        assert status == 200, (status, body)
        assert body.get("availableUserDomains") == [], body

        # ── Seed alice + provision her app credential (the fixture verifier)
        # over the User-class kind, connecting AS alice (self-scoped). ──
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=handle_domain, base_url=nest["url"],
        )
        alice_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=alice["actor_id_bytes"],
            signing_key=bytes(alice["signing_key"]),
        )
        with alice_ws:
            # Enter the hosted level first: since slice 4d, an account with no
            # ACTIVE hosted identity (`login_did` nil) joins createSession's
            # uniform-401 set, so F1's login round-trip needs S2's mint done.
            alice_ws.call(
                "fauna.bridges.atproto.set_integration_level",
                {
                    "target_level": "hosted_visible",
                    "did_method": "plc",
                    "user_rotation_pub_did_key": USER_ROTATION_PUB,
                    "history_backfill": False,
                },
            )
            reply = alice_ws.call(
                "fauna.bridges.atproto.provision_app_credential",
                {"credential_id": "ivory", "label": "Ivory", "verifier": verifier,
                 "dm_allowed": False},
            )
            assert reply.get("ok") is True, reply

            # Named generous budget + deadline poll (convention 14): the mint
            # loop's own poll tick is 30 s (`mint.go::mintPollInterval`), so
            # the ceiling covers two ticks under load; a green run pays only
            # the actual wait.
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                st = alice_ws.call(
                    "fauna.bridges.atproto.get_integration_status", {}
                )
                if (st.get("identity") or {}).get("status") == "active":
                    break
                assert proc.poll() is None, "bridge exited mid-mint:\n" + _tail()
                time.sleep(0.5)
            else:
                pytest.fail("hosted identity never became ACTIVE:\n" + _tail())

            # ── createSession: the raw XRPC login round-trip (F1 success). ──
            status, sess = _xrpc_post(
                xrpc_base, "com.atproto.server.createSession",
                {"identifier": "alice", "password": secret},
            )
            assert status == 200, f"createSession failed: {status} {sess}\n{_tail()}"
            assert sess["handle"] == "alice"
            # The session names the account's REAL ATProto identity since F2.2
            # slice 4d (the did:fauna placeholder -> did:plc swap) — the same
            # identity the mint just made ACTIVE above.
            assert sess["did"].startswith("did:plc:")
            access, refresh = sess["accessJwt"], sess["refreshJwt"]

            # getSession — the authed no-op that IS F1's definition of success.
            status, got = _xrpc_get(xrpc_base, "com.atproto.server.getSession", access)
            assert status == 200, f"getSession failed: {status} {got}"
            assert got["handle"] == "alice" and got["did"] == sess["did"]

            # ── Preferences round-trip (F3 phase 4 success): the session-authed
            # client writes then reads its private preferences, served LOCALLY
            # from nest state — a registered, non-Proxyable route that shadows
            # the headerless-AppView fallback, so the blob never leaves the box.
            # Opaque passthrough: the exact array comes back byte-for-byte. ──
            status, prefs0 = _xrpc_get(xrpc_base, "app.bsky.actor.getPreferences", access)
            assert status == 200, f"getPreferences (unset) failed: {status} {prefs0}"
            assert prefs0.get("preferences") == [], ("unset default must be []", prefs0)

            saved = [
                {"$type": "app.bsky.actor.defs#savedFeedsPrefV2",
                 "items": [{"type": "feed", "value": "at://x", "pinned": True, "id": "a"}]},
                {"$type": "app.bsky.actor.defs#adultContentPref", "enabled": False},
            ]
            status, _put = _xrpc_post(
                xrpc_base, "app.bsky.actor.putPreferences",
                {"preferences": saved}, access,
            )
            assert status == 200, f"putPreferences failed: {status} {_put}\n{_tail()}"

            status, prefs1 = _xrpc_get(xrpc_base, "app.bsky.actor.getPreferences", access)
            assert status == 200, (status, prefs1)
            assert prefs1.get("preferences") == saved, (
                "preferences did not round-trip verbatim", prefs1,
            )

            # The nest registry lists the session, and it is revocable there.
            sessions = alice_ws.call("fauna.bridges.atproto.list_sessions", {})["sessions"]
            assert len(sessions) == 1, sessions
            # session_id is a CBOR byte string on the wire (serde_bytes); the
            # client hands it back as Python bytes and re-encodes bytes → CBOR
            # bytes, so pass it through verbatim (never list()-ified).
            session_id = bytes(sessions[0]["session_id"])

            # A wrong secret is refused uniformly.
            status, err = _xrpc_post(
                xrpc_base, "com.atproto.server.createSession",
                {"identifier": "alice", "password": "aaaa-bbbb-cccc-dddd"},
            )
            assert status == 401 and err.get("error") == "AuthenticationRequired", (status, err)

            # A user-driven session revoke kills the derived session: the nudge
            # reaches the bridge, and getSession stops working within the bound.
            revoke = alice_ws.call(
                "fauna.bridges.atproto.revoke_session",
                {"session_id": session_id},
            )
            assert revoke.get("revoked") is True, revoke
            assert alice_ws.call("fauna.bridges.atproto.list_sessions", {})["sessions"] == []
            # (The access token's 60-min lifetime still verifies structurally —
            # its revocation bound is refresh + the kill-switch, which the next
            # step exercises directly.)

            # ── The external-apps kill-switch cuts the whole plane. Flip OFF →
            # the nudge reaches the bridge → a live access token stops working
            # and new logins are refused uniformly. ──
            alice_ws.call("fauna.bridges.atproto.set_external_apps_enabled", {"enabled": False})
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                status, _ = _xrpc_get(xrpc_base, "com.atproto.server.getSession", access)
                if status == 401:
                    break
                time.sleep(0.3)
            else:
                pytest.fail("kill-switch OFF nudge never cut the live access token:\n" + _tail())

            status, err = _xrpc_post(
                xrpc_base, "com.atproto.server.createSession",
                {"identifier": "alice", "password": secret},
            )
            assert status == 401 and err.get("error") == "AuthenticationRequired", (
                "kill-switch OFF must refuse createSession uniformly", status, err,
            )

            # Flip back ON → logins work again (non-destructive, reversible).
            alice_ws.call("fauna.bridges.atproto.set_external_apps_enabled", {"enabled": True})
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                status, sess2 = _xrpc_post(
                    xrpc_base, "com.atproto.server.createSession",
                    {"identifier": "alice", "password": secret},
                )
                if status == 200:
                    break
                time.sleep(0.3)
            else:
                pytest.fail("kill-switch ON never restored createSession:\n" + _tail())

            # ── Sealed-secret durability (the F1 remainder's success gate):
            # restart the bridge and a PRE-restart access token still
            # verifies. Only true if the HS256 secret is fetched + unsealed
            # from the nest-stored sealed blob — a per-process random secret
            # (the retired interim shape) fails exactly here. Re-Popen of the
            # identical argv is the conftest `respawn()` pattern (the
            # binaries e2e has no supervisor). ──
            access2 = sess2["accessJwt"]
            status, got = _xrpc_get(xrpc_base, "com.atproto.server.getSession", access2)
            assert status == 200, ("pre-restart sanity", status, got)

            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
            untrack_process(proc)
            log_fh.close()
            log_fh = open(log_path, "ab")
            proc = subprocess.Popen(
                [
                    atproto_bin,
                    f"--keypair-file={keyfile_path}",
                    f"--nest-endpoint={nest['url']}",
                    f"--data-dir={tmp.as_posix()}",
                    f"--xrpc-listen=127.0.0.1:{xrpc_port}",
                    "--log-level=debug",
                ],
                stdout=log_fh, stderr=subprocess.STDOUT, env=os.environ.copy(),
                **popen_group_kwargs(),
            )
            # Windows half of the die-with-the-run guarantee —
            # `popen_group_kwargs()` is `{}` there, so without this the
            # restarted bridge's only protection is the atexit sweep a killed
            # run never reaches (testing.md § point 9). No-op off Windows.
            reap_descendants_of(proc.pid)
            track_process(proc)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if log_path.read_text(errors="replace").count("xrpc listener up") >= 2:
                    break
                assert proc.poll() is None, "bridge exited on restart:\n" + _tail()
                time.sleep(0.5)
            else:
                pytest.fail("XRPC listener never came back after restart:\n" + _tail())

            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                status, got = _xrpc_get(
                    xrpc_base, "com.atproto.server.getSession", access2
                )
                if status == 200:
                    break
                time.sleep(0.3)
            else:
                pytest.fail(
                    "pre-restart access token no longer verifies after bridge "
                    f"restart (secret not durable?): {status} {got}\n" + _tail()
                )
            assert got["handle"] == "alice" and got["did"] == sess2["did"]
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
        if directory is not None:
            directory.close()
        nest_cleanup()
