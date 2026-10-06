"""Co-run regression: the standalone MTA and MDA bridge fixtures must coexist
on ONE nest (tracked internally).

All three mail-bridge fixtures are `scope="session"` against the one session
`nest_instance`. A nest has exactly ONE primary mail domain
(`mail-multidomain.md` § The primary domain — partial-unique-index enforced),
and BOTH bridge roles key their TLS cert on it (`cmd/fauna-mail-bridge/
main.go:500`: `Domain: snapshot.PrimaryDomain`, role-agnostic). Before the fix
the two standalone fixtures claimed *different* domains (`mta.fauna.test` /
`mda.fauna.test`); whichever instantiated second got a non-primary domain, so
its sealed cert blob — provisioned for its own domain — never matched the
runtime `PrimaryDomain`. The initial `tls.Refresh` then soft-failed
(`main.go:514-518`) and every TLS handshake on that bridge failed with
`INTERNAL_ERROR` ("no tls cert blob for (role=…, domain=<other-bridge's>)").
That made the full mail-bridge suite un-co-runnable (the prior workaround was
"run mail-bridge tests isolated").

This file co-instantiates both standalone fixtures and proves the MDA serves
IMAPS — which only holds once they share one primary domain (the production
shape). Run in isolation it is a deterministic RED→GREEN proof: the parameter
order (`mail_bridge_mta` first) makes the MTA claim primary, so the MDA is the
second / non-primary bridge whose TLS would break pre-fix.

Tier: tier_3 — real nest + real bridges + real seal/TLS; the only tier that
catches cross-binary cert keying.
"""

import time

import pytest

from helpers.mail_wire import _imap_auth_plain, _imap_read_tagged, _imaps_connect

pytestmark = pytest.mark.tier_3


def test_mda_imaps_handshake_survives_mta_corun(mail_bridge_mta, mail_bridge_mda):
    """MDA IMAPS handshake + AUTH + SELECT succeed when the MTA fixture
    instantiated first and claimed the nest's primary domain.

    `mail_bridge_mta` is listed first so it sets up first (pytest instantiates
    same-scope fixtures in request order) and wins `is_primary`. Previously the
    MDA then served `PrimaryDomain = mta.fauna.test` while holding a cert only
    for its own `mda.fauna.test` → handshake `INTERNAL_ERROR`. With one shared
    primary domain the MDA's cert matches and the handshake completes.

    The handshake is asserted first (the genuine pre-fix failure mode in the
    expected mta-first order); the domain-equality guard is a backstop that also
    catches the bug in the unlikely mda-first instantiation order.
    """
    mda = mail_bridge_mda
    deadline = time.monotonic() + 40.0

    # `_imaps_connect` performs the TLS handshake and raises ssl.SSLError on the
    # broken-cert path (the pre-fix RED). A clean handshake + AUTH + SELECT is
    # the GREEN.
    sock, buf = _imaps_connect(mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", mda.recipient_username, mda.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed over the MDA's primary-domain cert"
        sock.sendall(b"a2 SELECT INBOX\r\n")
        status, _ = _imap_read_tagged(sock, buf, "a2", deadline)
        assert status == "OK", f"SELECT INBOX must succeed; got {status}"
        sock.sendall(b"a3 LOGOUT\r\n")

    # Backstop: the one-primary-per-nest invariant — both standalone fixtures
    # claim the SAME primary mail domain (mail-multidomain.md § The primary
    # domain). This is what makes the cert keying agree regardless of which
    # fixture instantiated first.
    assert mail_bridge_mta.domain == mda.domain, (
        "the MTA and MDA fixtures must share ONE primary mail domain; got "
        f"mta={mail_bridge_mta.domain!r} mda={mda.domain!r}"
    )
