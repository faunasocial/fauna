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
"""

import subprocess
import sys
import time

import pytest

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("admin-bridges")
def test_revoked_keyfile_re_keys_in_process(
    mail_bridge_binary, nest_binary, tmp_path_factory
):
    import os

    import cbor2
    from nacl.signing import SigningKey as NaClSigningKey

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import (
        _apply_bridge_ffi_env,
        _make_nest,
        _wait_for_mail_bridge_metrics,
    )
    from helpers.bridge_enrollment import enroll_and_approve_bridge
    from drivers.port_util import find_free_ports, track_process, untrack_process

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
        keyfile_path = tmp / "mta.key"
        keyfile_path.write_bytes(
            cbor2.dumps(
                {
                    "v": 1,
                    "role": "mta",
                    "bridge_id": "rekey-mta",
                    "ed25519_seed": bytes(ed_sk),
                    "x25519_priv": os.urandom(32),  # never used post-revoke
                    "created_at": int(time.time()),
                },
                canonical=True,
            )
        )
        keyfile_path.chmod(0o600)
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
            from drivers.port_util import popen_group_kwargs, reap_descendants_of

            proc = subprocess.Popen(
                [
                    mail_bridge_binary,
                    f"--keypair-file={keyfile_path}",
                    f"--nest-endpoint={nest['url']}",
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
