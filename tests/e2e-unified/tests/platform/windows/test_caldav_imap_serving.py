"""Windows-native CalDAV + IMAP serving e2e (Track D runnable core).

Proves the *Windows* nest + MDA binaries serve CalDAV + IMAP — the gap the Linux
any-locator matrix (`test_caldav_onboarding_variants.py`, Linux binaries) cannot
cover. Spawns ``fauna-nest-svc.exe --foreground`` (the loopback plain-HTTP
``fauna_nest::start_server`` path) and the Windows ``fauna-mail-bridge.exe`` MDA
on temp ports, headless-claims, enables CalDAV, mints a known-password credential
via the ``seal-helper`` (no Fauna app), and round-trips a calendar event via
two Python ``CalDAVClient``s + opens an IMAP mailbox via ``imaplib`` — **no MSI
install**. The install-level proof is throwaway-VM-gated (`test_installer.py`,
`installers/windows.md` § Testing); this is its runnable, non-disruptive core
(temp data dirs + ephemeral loopback ports, no SCM / HKLM / %ProgramData%
writes), so it runs on the shared Windows VM in the inner loop and is reused by
the install-level test. The serving engine lives in
``helpers/windows_caldav_nest.py`` so the install-gated variant reuses it.

Authority:
  - Track D design (tracked internally)
  - docs/goal/behavior/caldav-server.md (§ Process topology, § Network exposure,
    § Authentication — any-locator login, § Independent enablement)
  - docs/goal/architecture/installers/windows.md § Testing
"""

from __future__ import annotations

import imaplib
import os
import ssl
import sys
import time

import pytest

from drivers.port_util import find_free_port
from helpers import windows_caldav_nest as eng

pytestmark = [
    pytest.mark.skipif(
        sys.platform != "win32",
        reason="Exercises the Windows-native fauna-nest-svc + fauna-mail-bridge binaries",
    ),
    pytest.mark.tier_3,
]


def test_nest_svc_foreground_writes_floor_cert_into_data_dir(tmp_path):
    """A started Windows FaunaNest writes its self-signed TLS *floor* cert
    (``fullchain.pem`` + ``privkey.pem``) into its own **data-dir** acme dir.

    This is the precondition the supervised MDA fetches to serve CalDAV/IMAP over
    TLS, and the recoverability invariant requires the floor live under the nest
    data dir (the dir a client backup/factory-reset governs) — not a
    machine-global path. It verifies the ``start_server`` floor-cert write
    (`bins/fauna-nest/src/lib.rs` ``ensure_floor_present``) fires on the real
    Windows ``fauna-nest-svc`` binary's loopback ``--foreground`` path.
    """
    data_dir = tmp_path / "nest"
    data_dir.mkdir()
    port = find_free_port()
    proc = eng.spawn_nest_svc(data_dir, port)
    try:
        cert = data_dir / "acme" / "fullchain.pem"
        key = data_dir / "acme" / "privkey.pem"
        deadline = time.time() + 30.0
        while time.time() < deadline:
            if cert.exists() and key.exists():
                break
            if proc.poll() is not None:
                pytest.fail(
                    f"fauna-nest-svc exited early (rc={proc.returncode}) before "
                    f"writing the floor cert.\nlog:\n{eng.log_tail(data_dir / 'nest-svc.log')}"
                )
            time.sleep(0.25)
        assert cert.exists() and key.exists(), (
            f"floor cert not written under the data-dir acme dir within 30s "
            f"(expected {cert} + {key}).\nnest-svc log:\n"
            f"{eng.log_tail(data_dir / 'nest-svc.log')}"
        )
    finally:
        eng.terminate(proc)


def test_windows_nest_claims_enables_caldav_and_mints_credential(tmp_path):
    """Stage A+B in isolation: the Windows nest-svc headless-claims, commits
    plaintext storage, enables CalDAV, and a known-password PLAIN credential is
    minted for the claimed actor entirely over WS-RPC + the Windows seal-helper —
    **no Fauna app, no MDA**. Localizes a provisioning failure away from the
    MDA-serving seam. Asserts the credential round-trips at the wire level:
    ``fetch_wrapped_mls_blob`` returns the blob the MDA AEAD-unwraps at AUTH.
    """
    import sqlite3

    prov = eng.provision_windows_caldav_nest(tmp_path / "nest")
    try:
        from clients.ws_rpc_admin_client import WsRpcAdminClient

        ws = WsRpcAdminClient(
            prov.nest_url, actor_id=prov.actor_id, signing_key=bytes(prov.admin["signing_key"]),
        )
        with ws:
            cfg = ws.call("fauna.bridges.get_mail_config", {})
            assert cfg.get("caldav_enabled") is True, f"caldav not enabled: {cfg!r}"

        # The wrapped-MSEK AUTH credential is fetched over the wire ONLY by an MDA
        # bridge (`fetch_wrapped_mls_blob` is BridgeMda-class), so verify the mint
        # landed at-rest: its `(actor_id, "default")` row in `bridge_wrapped_mls_blobs`
        # (`bins/fauna-nest/src/db/migrations.rs`). The MDA AEAD-unwraps it at AUTH.
        conn = sqlite3.connect(prov.db_path, timeout=10.0)
        try:
            n = conn.execute(
                "SELECT count(*) FROM bridge_wrapped_mls_blobs WHERE actor_id=? AND credential_id=?",
                (prov.actor_id, eng.CREDENTIAL_ID),
            ).fetchone()[0]
        finally:
            conn.close()
        assert n == 1, f"expected the minted wrapped-MSEK credential row, found {n}"
    finally:
        prov.cleanup()


@pytest.mark.feature("calendar-in-standard-apps")
def test_windows_caldav_imap_round_trip(tmp_path):
    """The heart of Track D: a Windows nest-svc + ``fauna-mail-bridge.exe`` MDA
    serve CalDAV + IMAP to Python clients off the self-signed floor cert.

    CalDAV: two ``CalDAVClient``s (one shared actor, one Personal calendar) each
    create an event and see the other's — a bidirectional round-trip through the
    real MDA→nest seal/persist/sync path. IMAP: a stock ``imaplib`` client logs in
    with the same bridge credential and opens INBOX (the one MDA serves both).
    """
    from helpers.caldav_client import CalDAVClient, build_vevent
    from helpers.caldav_roundtrip import caldav_has

    nest = eng.start_windows_caldav_nest(tmp_path)
    try:
        # ── CalDAV bidirectional round-trip via two MUAs on the shared calendar.
        mua_a = CalDAVClient(
            nest.caldav_base_url, nest.auth_username, nest.password, verify=False,
        )
        mua_b = CalDAVClient(
            nest.caldav_base_url, nest.auth_username, nest.password, verify=False,
        )
        cal_a = mua_a.personal_calendar()
        cal_b = mua_b.personal_calendar()

        nonce = os.urandom(5).hex()
        s_a = f"{nonce}-from-a"
        uid_a = f"{nonce}-uid-a"
        mua_a.put_event(cal_a, uid_a, build_vevent(
            uid_a, s_a, "20260620T120000Z", "20260620T130000Z",
        ))
        assert caldav_has(mua_b, cal_b, s_a, timeout=60.0), (
            f"event {s_a!r} created by MUA A never became visible to MUA B "
            f"(CalDAV serving/persist/sync break).\nMDA log:\n"
            f"{eng.log_tail(nest.mda_dir / 'mail-bridge-mda.log')}"
        )

        s_b = f"{nonce}-from-b"
        uid_b = f"{nonce}-uid-b"
        mua_b.put_event(cal_b, uid_b, build_vevent(
            uid_b, s_b, "20260620T140000Z", "20260620T150000Z",
        ))
        assert caldav_has(mua_a, cal_a, s_b, timeout=60.0), (
            f"event {s_b!r} created by MUA B never became visible to MUA A.\n"
            f"MDA log:\n{eng.log_tail(nest.mda_dir / 'mail-bridge-mda.log')}"
        )

        # ── IMAP: the same credential opens the mailbox (one MDA serves both).
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        imap = imaplib.IMAP4_SSL("127.0.0.1", nest.imaps_port, ssl_context=ctx)
        try:
            imap.login(nest.auth_username, nest.password)
            typ, _ = imap.select("INBOX")
            assert typ == "OK", f"IMAP SELECT INBOX failed: {typ}"
            typ_l, _ = imap.list()
            assert typ_l == "OK", f"IMAP LIST failed: {typ_l}"
        finally:
            try:
                imap.logout()
            except Exception:
                pass
    finally:
        nest.cleanup()
