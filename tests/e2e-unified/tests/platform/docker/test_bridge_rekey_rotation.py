"""tier_4 e2e: **service-user re-keying completes hands-free on the real image**
(mail-bridge-lifecycle.md § Service-user re-keying, built 2026-07-09).

The admin revokes the MTA's service user ("Rotate bridge service-user key");
the rotation must then complete with NO container recreate and NO shell step:

  1. The running bridge notices within one periodic-RPC interval — nest's
     central gate denies every kind for the revoked actor, and the
     ReconnectingClient's standing probe (whoami on the same live connection,
     also denied) maps that to the revoke shutdown; s6 restarts it (step 4-5;
     the probe is `internal/wsrpc/reconnect.go::probeStanding`).
  2. At the cold-boot `request_enrollment` poll the bridge sees `revoked`,
     archives the old keypair at `<keypair>.revoked.<unix-time>` (0400) and
     generates a fresh one (steps 6–7, `keypair.ArchiveAndRegenerate`).
  3. Strict mode (this image provisions the blessed registry): the fresh key's
     first enrollment is rejected `permission_denied` (not yet blessed), the
     bridge exits 0, s6 restarts it, and the run-script's
     `rebless-bridge-key.sh` (root, pre-drop) re-derives the blessed pubkey
     from the keyfile; nest re-reads the registry file live per enrollment
     (`FAUNA_BLESSED_KEYS_DIR`), so the re-bless lands with no nest restart.
  4. Mail is enabled, so the fresh pending enrollment auto-approves
     (§ Onboarding auto-approval) and the MTA returns to serving (step 8).

Only tier_4 can prove this: the loop spans the s6 supervisor restarts, the
root-mediated run-script re-bless, and the nest run-script's registry-dir env
wiring — all packaging that the binary-spawning tier_3 fixtures bypass. The
lenient (no-registry) in-process leg is tier_3
(`tests/test_mail_bridge_rekey.py`); the archive/regenerate file semantics are
Go unit tests (`internal/keypair/keyfile_test.go::TestArchiveAndRegenerate`).

Serving proof after the re-key = the SMTP banner on :25 + the approved row
carrying the NEW pubkey; the full mail round-trip on this same image is the
core-8 suite's job (`test_mail_deploy_*`), not re-proven here.
"""

import re
import subprocess
import time

import pytest

from .helpers import ROLES

# Reuse the PoP module's claimed + mail-enabled + both-bridges-approved fixture
# chain (module-scoped `serving_nest` + `docker_image`) — same bring-up, and
# rotation composes on top of the strict-enrollment state that module proves.
from .test_bridge_enrollment_pop import (  # noqa: F401  (pytest fixture re-export)
    _exec_as,
    _read_as_root,
    _stat,
    docker_image,
    serving_nest,
)
from .test_bridge_enrollment_pop import (
    KEYFILE,
    MTA_UID,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
    pytest.mark.self_contained_docker,
]

ARCHIVE_RE = re.compile(r"\Amta\.key\.revoked\.\d+(\.\d+)?\Z")


def _service_users(admin, status: str) -> list[dict]:
    return admin.call("fauna.bridges.list_service_users", {"status": status})[
        "service_users"
    ]


def _approved_by_role(admin) -> dict[str, dict]:
    return {b["role"]: b for b in _service_users(admin, "approved")}


def _smtp_banner(port: int, timeout: float = 10.0) -> str:
    import socket

    with socket.create_connection(("127.0.0.1", port), timeout=timeout) as s:
        s.settimeout(timeout)
        return s.recv(256).decode("utf-8", "replace")


@pytest.mark.feature("admin-bridges")
def test_admin_revoke_re_keys_mta_hands_free(serving_nest):
    """Revoke the MTA → archived keypair + fresh auto-approved identity +
    re-blessed registry + SMTP serving again, all with no shell step."""
    from .helpers import admin_ws as _admin_ws

    name = serving_nest["name"]
    mta_keyfile = KEYFILE["mta"]
    keydir = "/data/keys/mta"

    with _admin_ws(serving_nest) as admin:
        before = _approved_by_role(admin)
        assert set(ROLES) <= set(before), f"precondition: both approved, got {list(before)}"
        old_mta_pk: bytes = bytes(before["mta"]["ed25519_pubkey"])
        old_mda_pk: bytes = bytes(before["mda"]["ed25519_pubkey"])
        old_blessed = _read_as_root(name, "/data/keys/blessed/mta.pub")

        # ── Rotate: the admin's one action. ──
        reply = admin.call(
            "fauna.bridges.revoke_service_user", {"bridge_actor_id": old_mta_pk}
        )
        assert reply.get("ok") is True

        # NOTHING else — the whole loop is hands-free from here: the running
        # bridge's next periodic RPC gets the central-gate permission_denied,
        # the ReconnectingClient's standing probe (whoami also denied) turns it
        # into the revoke shutdown (≤ ~30 s, the outbound-poll interval), s6
        # restarts it, and the cold-boot re-key + re-bless loop runs.

        # ── The whole loop runs unattended; wait for the NEW approved MTA. ──
        deadline = time.monotonic() + 180
        new_mta = None
        while time.monotonic() < deadline:
            by_role = _approved_by_role(admin)
            cand = by_role.get("mta")
            if cand is not None and bytes(cand["ed25519_pubkey"]) != old_mta_pk:
                new_mta = cand
                break
            time.sleep(2.0)
        assert new_mta is not None, (
            "a FRESH auto-approved MTA identity must appear after the revoke "
            f"(old still sole approved after 180s). docker logs tail:\n"
            + subprocess.run(
                ["docker", "logs", "--tail", "80", name],
                capture_output=True, text=True, timeout=15,
            ).stdout
        )
        new_mta_pk = bytes(new_mta["ed25519_pubkey"])

        # The old identity stays revoked (never resurrected); the MDA is
        # untouched.
        revoked_pks = {bytes(b["ed25519_pubkey"]) for b in _service_users(admin, "revoked")}
        assert old_mta_pk in revoked_pks, "the revoked MTA row must stay revoked"
        assert bytes(_approved_by_role(admin)["mda"]["ed25519_pubkey"]) == old_mda_pk, \
            "an MTA rotation must not disturb the MDA's enrollment"

        # The fresh identity attested its x25519 (register_service_user ran →
        # it is a valid seal target for the re-minted TLS blob).
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            row = _approved_by_role(admin)["mta"]
            if row["has_x25519"]:
                break
            time.sleep(2.0)
        assert _approved_by_role(admin)["mta"]["has_x25519"], \
            "the re-keyed MTA must attest its x25519 (cold boot register_service_user)"

    # ── Disk state: the archive exists (0400, mta-owned) and the keyfile is
    # the fresh key. ──
    rc, out, err = _exec_as(name, 0, "ls", keydir)
    assert rc == 0, f"ls {keydir}: {err}"
    entries = out.decode().split()
    archives = [e for e in entries if ARCHIVE_RE.match(e)]
    assert archives, f"an mta.key.revoked.<ts> archive must exist; got {entries}"
    st = _stat(name, f"{keydir}/{archives[0]}")
    assert st is not None and st[2] == "400", \
        f"the archived keypair must be mode 0400; got {st}"

    # The keyfile's pubkey == the newly-approved identity (--print-pubkey is
    # load-only), and the blessed registry was re-derived to it (root-mediated
    # re-bless) — no longer the old blessing.
    rc, out, err = _exec_as(
        name, MTA_UID, "fauna-mail-bridge", "--keypair-file", mta_keyfile, "--print-pubkey"
    )
    assert rc == 0, f"--print-pubkey on the fresh keyfile: {err}"
    fresh_keyfile_pub = out.decode("ascii", "strict").strip()
    assert fresh_keyfile_pub == new_mta_pk.hex(), \
        "the on-disk keyfile must hold the newly-approved identity"
    new_blessed = _read_as_root(name, "/data/keys/blessed/mta.pub")
    assert new_blessed == fresh_keyfile_pub != old_blessed, \
        "the blessed registry must be re-derived to the fresh pubkey"

    # ── Serving proof: the MX greets again on :25 (the port the rotation
    # bounced). Full round-trip coverage on this image = the core-8 suite. ──
    deadline = time.monotonic() + 60
    banner = ""
    while time.monotonic() < deadline:
        try:
            banner = _smtp_banner(serving_nest["mail_ports"][25])
            if banner.startswith("220"):
                break
        except OSError:
            pass
        time.sleep(2.0)
    assert banner.startswith("220"), \
        f"the re-keyed MTA must serve SMTP again; last banner {banner!r}"
