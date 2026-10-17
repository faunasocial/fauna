"""tier_3: the **lenient-mode service-user re-key completes in ONE process run**
(mail-bridge-lifecycle.md § Service-user re-keying steps 6–7).

A bridge cold-booting on a keypair whose enrollment row is REVOKED must — with
no blessed registry provisioned (the dev/binary lenient path) — archive the old
keypair at ``<keypair>.revoked.<unix-time>`` (0400), generate a fresh one at
the original path, and re-enroll **in-process** (no supervisor restart exists
here to lean on); with deployment mail enabled the fresh enrollment
auto-approves (§ Onboarding auto-approval) and the bridge proceeds to running.

Pre-fix (the rotation dead-loop this locks against): the bridge exited 0 on the
``revoked`` reply and re-enrolled the same revoked pubkey forever — the ONLY
way out was hand-deleting the key file, which the product invariants forbid.

The strict-mode leg (blessed registry + root-mediated re-bless + s6 restarts)
is tier_4 ``tests/platform/docker/test_bridge_rekey_rotation.py``; the archive
file semantics are Go unit tests (``keypair::TestArchiveAndRegenerate``).

Two more arms of the same flow (``docs/features/admin-bridges.md``):

- **admin-bridges 15** — the mail-off arm: with mail never switched on, the
  regenerated key does NOT approve itself; it lands as a fresh pending card and
  the bridge waits in its enrollment poll (§ Onboarding auto-approval: the MTA
  approves itself only once mail is on).
- **admin-bridges 18** — mail flows after the rotation: a running MTA serving
  the shared mail venue is revoked, exits on the ``4401`` (step 4), is restarted
  the way its supervisor would (step 5), re-keys (steps 6–7), and then takes an
  inbound message to a local recipient and a submission that reaches the
  external stub MX.
"""

import subprocess
import sys
import time

import pytest

pytestmark = pytest.mark.tier_3


def _write_mta_keyfile(tmp, ed_sk, bridge_id: str):
    """The bridge's keyfile for ``ed_sk`` at ``<tmp>/mta.key`` (the basename is
    the role hint the bridge's self-enrollment sends)."""
    import os

    import cbor2

    keyfile_path = tmp / "mta.key"
    keyfile_path.write_bytes(
        cbor2.dumps(
            {
                "v": 1,
                "role": "mta",
                "bridge_id": bridge_id,
                "ed25519_seed": bytes(ed_sk),
                "x25519_priv": os.urandom(32),  # never used post-revoke
                "created_at": int(time.time()),
            },
            canonical=True,
        )
    )
    keyfile_path.chmod(0o600)
    return keyfile_path


def _cold_boot(mail_bridge_binary, nest_url: str, keyfile_path, tmp):
    """Start the bridge on ``keyfile_path`` with loopback listeners; returns
    ``(proc, log_fh, log_path, metrics_port)``. The caller stops it with
    :func:`_stop`."""
    import os

    from conftest import _apply_bridge_ffi_env
    from drivers.port_util import (
        find_free_ports,
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
    )

    mx, s465, s587, metrics = find_free_ports(4)
    hatch = tmp / "operator-hatch.toml"
    hatch.write_text(
        f'data_dir = "{tmp.as_posix()}"\n'
        f'mta_bind_addr = "127.0.0.1:{mx}"\n'
        f'mta_bind_addr_465 = "127.0.0.1:{s465}"\n'
        f'mta_bind_addr_587 = "127.0.0.1:{s587}"\n'
        f'metrics_bind_addr = "127.0.0.1:{metrics}"\n'
    )
    env = os.environ.copy()
    _apply_bridge_ffi_env(env)
    log_path = tmp / "bridge.log"
    log_fh = open(log_path, "wb")
    proc = subprocess.Popen(
        [
            mail_bridge_binary,
            f"--keypair-file={keyfile_path}",
            f"--nest-endpoint={nest_url}",
            f"--operator-hatch={hatch}",
            "--log-level=debug",
        ],
        stdout=log_fh,
        stderr=subprocess.STDOUT,
        env=env,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — no-op off Windows.
    reap_descendants_of(proc.pid)
    track_process(proc)
    return proc, log_fh, log_path, metrics


def _stop(proc, log_fh) -> None:
    from drivers.port_util import untrack_process

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


@pytest.mark.feature("admin-bridges")
def test_revoked_keyfile_re_keys_in_process(
    mail_bridge_binary, nest_binary, tmp_path_factory
):
    import cbor2
    from nacl.signing import SigningKey as NaClSigningKey

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _make_nest, _wait_for_mail_bridge_metrics
    from helpers.bridge_enrollment import enroll_and_approve_bridge

    nest, nest_cleanup = _make_nest(nest_binary, tmp_path_factory, "rekey-nest")
    proc = None
    log_fh = None
    try:
        admin = nest["admin"]
        # ── A keypair whose row is REVOKED (the post-rotation cold-boot state):
        # enroll it (request_enrollment + approve, as the bridge's own first boot would) then revoke.
        ed_sk = NaClSigningKey.generate()
        old_pubkey = bytes(ed_sk.verify_key)
        tmp = tmp_path_factory.mktemp("rekey-bridge")
        keyfile_path = _write_mta_keyfile(tmp, ed_sk, "rekey-mta")
        enroll_and_approve_bridge(
            nest["url"], admin["signing_key"], old_pubkey, "mta", "rekey-mta",
        )

        admin_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            # A local mail domain so the post-re-key MTA has listeners to bind
            # (an empty `local_domains` projection binds nothing).
            admin_ws.call(
                "fauna.bridges.add_local_domain",
                {"domain": "fauna.test",
                 "mta_sts_cert_mode": "per_host"},
            )
            # Auto-approve of the regenerated key needs the explicit toggle
            # (`request_enrollment` gates on Some(true), not the fetch_config
            # unset-defaults-true default).
            admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})
            reply = admin_ws.call(
                "fauna.bridges.revoke_service_user", {"bridge_actor_id": old_pubkey}
            )
            assert reply.get("ok") is True

            # ── Cold boot on the revoked keypair. ──
            proc, log_fh, log_path, metrics = _cold_boot(
                mail_bridge_binary, nest["url"], keyfile_path, tmp
            )

            # ── The re-key runs unattended: a FRESH approved MTA appears. ──
            deadline = time.monotonic() + 60
            new_row = None
            while time.monotonic() < deadline:
                rows = admin_ws.call(
                    "fauna.bridges.list_service_users", {"status": "approved"}
                )["service_users"]
                fresh = [
                    r for r in rows
                    if r["role"] == "mta" and bytes(r["ed25519_pubkey"]) != old_pubkey
                ]
                if fresh:
                    new_row = fresh[0]
                    break
                assert proc.poll() is None, (
                    "the bridge must NOT exit on the revoked reply in lenient "
                    "mode (the pre-fix dead-loop); log:\n"
                    + log_path.read_text(errors="replace")[-4000:]
                )
                time.sleep(1.0)
            assert new_row is not None, (
                "a fresh auto-approved MTA identity must appear; log:\n"
                + log_path.read_text(errors="replace")[-4000:]
            )

            # The old row stays revoked — never resurrected.
            revoked = admin_ws.call(
                "fauna.bridges.list_service_users", {"status": "revoked"}
            )["service_users"]
            assert any(bytes(r["ed25519_pubkey"]) == old_pubkey for r in revoked)

        # ── In-process: the SAME process did the whole re-key (no restart). ──
        assert proc.poll() is None, "the re-key must complete within one process run"
        # ...and it proceeds to running (metrics listener up = post-approval).
        _wait_for_mail_bridge_metrics(metrics, timeout=30.0)

        # ── Disk state: archive (0400) + fresh keyfile holding the new key. ──
        archives = sorted(tmp.glob("mta.key.revoked.*"))
        assert archives, f"expected an mta.key.revoked.<ts> archive in {tmp}"
        # What `mode 0400` is FOR here is read-only preservation for
        # forensics/audit (`mail-bridge-lifecycle.md` § Service-user re-keying
        # step 7 — "preserved for forensic / audit purposes"; the archive is
        # deliberately never auto-deleted). So the invariant to assert is that
        # nothing can write it, and the exact POSIX triad is the way a POSIX
        # filesystem spells that — not the guarantee itself.
        #
        # Windows cannot spell it: NTFS security is ACL-based and the file
        # attributes carry one read-only flag, which Go's `os.Chmod` sets and
        # `stat` synthesizes back as 0o444 (r--r--r--) — never 0o400. The
        # product already ruled on exactly this, for the LIVE keyfile holding
        # the same two long-lived secrets: `keypair/keyfile.go` skips its
        # 0o077-clean gate on Windows because the check is "both meaningless
        # and unsatisfiable there", and names filesystem ACLs (the per-user
        # `%LOCALAPPDATA%` / service data dir) as the protection instead.
        # Hardening the DEAD archive past what guards the live key would be
        # incoherent, so this follows the same rule rather than inventing a
        # second one.
        mode = archives[0].stat().st_mode & 0o777
        if sys.platform == "win32":
            assert mode & 0o222 == 0, (
                f"the archived revoked key must be read-only (mode {mode:#o}); "
                "on Windows that is the single read-only attribute, and access "
                "control proper is the NTFS ACL, which mode bits cannot express"
            )
        else:
            assert mode == 0o400, f"archive mode {mode:#o}, want 0o400"
        archived = cbor2.loads(archives[0].read_bytes())
        assert archived["ed25519_seed"] == bytes(ed_sk), \
            "the archive must preserve the revoked key (audit/forensics)"
        fresh_kf = cbor2.loads(keyfile_path.read_bytes())
        fresh_pub = bytes(NaClSigningKey(fresh_kf["ed25519_seed"]).verify_key)
        assert fresh_pub == bytes(new_row["ed25519_pubkey"]), \
            "the on-disk keyfile must hold exactly the newly-approved identity"
    finally:
        _stop(proc, log_fh)
        nest_cleanup()


@pytest.mark.feature("admin-bridges")
def test_mail_off_re_key_comes_back_pending(mail_bridge_binary, nest_binary, tmp_path_factory):
    """admin-bridges 15: the mail-off arm of the in-process re-key. On a fresh
    nest where mail was never switched on, the bridge cold-boots on its revoked
    key, archives it, mints a fresh one — and that fresh key shows in the
    admin's pending feed and nowhere in the approved roster, while the bridge
    stays up in its enrollment poll waiting for the admin. The old key stays
    revoked and the keyfile on disk holds exactly the pending identity, so the
    pending card is this bridge's re-key and not some other enrollment."""
    import cbor2
    from nacl.signing import SigningKey as NaClSigningKey

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _make_nest
    from helpers.bridge_enrollment import enroll_and_approve_bridge
    from helpers.waiting import wait_until

    nest, nest_cleanup = _make_nest(nest_binary, tmp_path_factory, "rekey-mail-off-nest")
    proc = None
    log_fh = None
    try:
        admin = nest["admin"]
        ed_sk = NaClSigningKey.generate()
        old_pubkey = bytes(ed_sk.verify_key)
        tmp = tmp_path_factory.mktemp("rekey-mail-off-bridge")
        keyfile_path = _write_mta_keyfile(tmp, ed_sk, "rekey-mta")
        enroll_and_approve_bridge(
            nest["url"], admin["signing_key"], old_pubkey, "mta", "rekey-mta",
        )
        admin_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            # Mail is never switched on: the one difference from the arm above.
            assert admin_ws.call(
                "fauna.bridges.revoke_service_user", {"bridge_actor_id": old_pubkey}
            ).get("ok") is True

            proc, log_fh, log_path, _metrics = _cold_boot(
                mail_bridge_binary, nest["url"], keyfile_path, tmp
            )

            def _fresh(rows):
                return [
                    r for r in rows
                    if r["role"] == "mta" and bytes(r["ed25519_pubkey"]) != old_pubkey
                ]

            def _log_tail():
                return log_path.read_text(errors="replace")[-4000:]

            def _pending_re_key():
                assert proc.poll() is None, (
                    "the bridge must re-key, not exit, on its revoked key; log:\n" + _log_tail()
                )
                fresh = _fresh(admin_ws.call("fauna.bridges.list_pending_bridges", {})["bridges"])
                return fresh[0] if fresh else None

            pending_row = wait_until(
                _pending_re_key,
                60,
                interval=1.0,
                diagnose=lambda: "the re-keyed MTA never came back as a pending card; log:\n"
                + _log_tail(),
            )
            assert pending_row["status"] == "pending", pending_row

            approved = admin_ws.call(
                "fauna.bridges.list_service_users", {"status": "approved"}
            )["service_users"]
            assert not _fresh(approved), (
                "with mail off the re-keyed MTA must wait for the admin, never approve itself"
            )
            revoked = admin_ws.call(
                "fauna.bridges.list_service_users", {"status": "revoked"}
            )["service_users"]
            assert any(bytes(r["ed25519_pubkey"]) == old_pubkey for r in revoked)

        assert proc.poll() is None, "a pending bridge waits in its enrollment poll; it does not exit"
        fresh_kf = cbor2.loads(keyfile_path.read_bytes())
        fresh_pub = bytes(NaClSigningKey(fresh_kf["ed25519_seed"]).verify_key)
        assert fresh_pub == bytes(pending_row["ed25519_pubkey"]), \
            "the pending card must be this bridge's regenerated identity"
    finally:
        _stop(proc, log_fh)
        nest_cleanup()


@pytest.mark.feature("admin-bridges", "mail-server")
def test_mail_flows_after_the_mta_is_re_keyed(
    mail_bridge_mta, mail_bridge_binary, seal_helper_binary, nest_instance, tmp_path_factory
):
    """admin-bridges 18: a second MTA joins the shared mail venue (mail on, a
    local recipient, a submission sender, the external stub MX routed by its
    operator-hatch), and the admin revokes it. It exits on the ``4401`` (step 4);
    the test restarts it as its supervisor would (step 5); it re-keys and the
    fresh key approves itself (mail is on). Then, through THIS bridge's own
    listeners: an inbound message to the local recipient is accepted at the end
    of DATA (the bridge 250s only once the nest has stored it), and a submission
    from the local sender is accepted and reaches the stub MX.

    mail-server 30: the message that leaves after the re-key is signed with the
    SAME DKIM key — every signature on it verifies (dkimpy, an independent
    verifier) against the records the nest published BEFORE the revoke, and the
    published records are unchanged after it (`mail-bridge-lifecycle.md`
    § Service-user re-keying: a re-key touches no DKIM key).

    The outbound queue is the nest's, so the session MTA's worker may be the
    one that hands the message to the stub MX; what this test pins is that the
    re-keyed bridge authenticates the sender and enqueues — both nest calls
    under its new key — and that the message leaves."""
    import uuid

    from conftest import _spawn_mta_bridge
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd

    session = mail_bridge_mta
    domain = session.domain
    bridge_id = f"rekey-flow-{uuid.uuid4().hex[:12]}"
    # Inbound DATA passes the content-scan gate, which tempfails without a
    # scanner; the session bridge's fakes are not on its handle, so this bridge
    # gets its own pair (the same benign-by-default daemons).
    fake_clamd = FakeClamd().start()
    fake_rspamd = FakeRspamd().start()
    spawned = None
    try:
        spawned = _spawn_mta_bridge(
            mail_bridge_binary=mail_bridge_binary,
            seal_helper_binary=seal_helper_binary,
            nest_instance=nest_instance,
            tmp=tmp_path_factory.mktemp("rekey-flow-bridge"),
            bridge_id=bridge_id,
            domain=domain,
            operator_hatch_extra=(
                f'clamd_addr = "{fake_clamd.addr}"\n'
                f'rspamd_url = "{fake_rspamd.url}"\n'
                f'\n[mta_mx_override]\n"external.test" = "{session.stub_mx.target}"\n'
            ),
        )
        _re_key_then_mail_flows(session, spawned, nest_instance, bridge_id)
    finally:
        if spawned is not None:
            spawned.cleanup()
        fake_clamd.stop()
        fake_rspamd.stop()


def _re_key_then_mail_flows(session, spawned, nest_instance, bridge_id: str) -> None:
    """The body of :func:`test_mail_flows_after_the_mta_is_re_keyed`, on a
    spawned bridge the caller tears down."""
    import base64
    import uuid

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.mail_wire import _connect_smtp_starttls, _connect_submission_tls, _wait_for_tagged

    domain = session.domain
    old_pubkey = spawned.ed25519_pubkey
    admin = nest_instance["admin"]
    admin_ws = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )

    def published() -> dict[str, str]:
        """Every DKIM record the nest publishes for the domain, selector → TXT."""
        with admin_ws:
            rows = admin_ws.call("fauna.bridges.list_dkim_selectors", {"domain": domain})
        return {r["selector"]: r["public_dns_value"] for r in rows["selectors"]}

    published_before = published()
    with admin_ws:
        assert admin_ws.call(
            "fauna.bridges.revoke_service_user", {"bridge_actor_id": old_pubkey}
        ).get("ok") is True

    try:
        rc = spawned.proc.wait(timeout=60)
    except subprocess.TimeoutExpired:
        raise AssertionError(
            "the revoked MTA must shut down on the 4401; still running. Log: "
            + spawned.log_file_path
        )
    assert rc == 0, f"a revoke is a graceful shutdown, got exit {rc}; log: {spawned.log_file_path}"

    spawned.respawn(wait_for_serving=True, timeout=60)

    with admin_ws:
        approved = admin_ws.call(
            "fauna.bridges.list_service_users", {"status": "approved"}
        )["service_users"]
    mine = [r for r in approved if r["bridge_id"] == bridge_id]
    assert mine and all(bytes(r["ed25519_pubkey"]) != old_pubkey for r in mine), (
        f"the restarted bridge must serve under a fresh approved key; rows: {mine!r}"
    )

    token = f"rekey-{uuid.uuid4().hex[:12]}"
    deadline = time.monotonic() + 30.0

    # ── A message arrives. ──
    recipient = f"{session.recipient_local_part}@{domain}"
    inbound = "\r\n".join([
        "From: External Sender <sender@external.test>",
        f"To: {recipient}",
        f"Subject: after the re-key {token}",
        f"Message-ID: <in-{token}@external.test>",
        "Date: Tue, 13 Oct 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Inbound through the re-keyed MTA.",
    ]) + "\r\n"
    with _connect_smtp_starttls(spawned.mx_port, domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(inbound.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    # ── A message leaves. ──
    sender = f"{session.submission_sender_local}@{domain}"
    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + session.submission_credential
    ).decode()
    outbound = "\r\n".join([
        f"From: Sender <{sender}>",
        "To: recipient@external.test",
        f"Subject: after the re-key {token}",
        f"Message-ID: <out-{token}@{domain}>",
        "Date: Tue, 13 Oct 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Outbound through the re-keyed MTA.",
    ]) + "\r\n"
    with _connect_submission_tls(spawned.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd("RCPT TO:<recipient@external.test>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(outbound.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    received = _wait_for_tagged(session.stub_mx, token, timeout=30.0)
    assert received is not None, (
        f"the stub external MX received no message tagged {token}: the submission "
        f"through the re-keyed MTA never left (bridge log: {spawned.log_file_path})"
    )

    # ── It leaves signed with the same key: the published records still verify it. ──
    from helpers.mail_wire import dkim_signature_tags, verify_each_dkim_signature

    signed_with = dkim_signature_tags(received, "s")
    verified, queries = verify_each_dkim_signature(received, domain, published_before)
    assert signed_with and verified == signed_with, (
        f"after the re-key every signature ({signed_with!r}) must verify against the "
        f"records published before it; verified {verified!r}, dnsfunc queries {queries!r}"
    )
    assert published() == published_before, (
        "a service-user re-key must leave the published DKIM records untouched"
    )
