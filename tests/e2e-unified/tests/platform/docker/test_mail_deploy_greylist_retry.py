"""tier_4: greylist-then-retry-delivered — the inbound perimeter ENABLED.

The user hit this live (2026-06-04): an inbound reply to example.com was
greylist-tempfailed (`smtp: greylist tempfail`) and the diagnosis was "the sending
server will retry and it'll deliver." That retry-delivers claim had **zero** test
coverage: every other docker mail test DISABLES greylist (`relax_spam_policy` sets
`greylist_enabled=False`), so the perimeter-on inbound path — the one production
actually runs — was never exercised end-to-end. This locks the full cycle in the
real deploy image:

  1. greylist ENABLED (short 2 s delay) — only the greylist gate is on; dnsbl /
     fcrdns / conn-rate stay relaxed so the synthetic loopback peer clears them.
  2. first inbound (loopback STARTTLS, port 25) → **451 4.7.1 at RCPT** (the
     server.go "Greylisted; try again later" tempfail) — NOT stored.
  3. wait past the delay window, retry the SAME (peer, MAIL FROM, RCPT TO) triplet
     → **250 accept**.
  4. IMAPS read → the message comes back **decrypted**, and the INBOX holds exactly
     ONE message (the greylisted first attempt left no copy).

Greylisting is enforced nest-side (`fauna.bridges.check_greylist`,
`libs/fauna-mail/src/greylist.rs`; mta.go § Greylisting) keyed on the triplet, so
it applies to the loopback delivery too — which is exactly why the other tests
relax it. See `docs/goal/architecture/testing.md` § Gap 2 (the mail-flow
perimeter). tier_4 (mocking depth): real deploy image + s6-supervised MTA/MDA +
fake clamd/rspamd sidecars; only the image exercises the nest-side greylist state
+ the bridge's 451 mapping together.
"""

import time

import pytest

from .helpers import (
    admin_ws,
    bridge_diag,
    bring_bridges_to_serving,
    deliver_inbound_attempt_loopback_curl,
    deliver_inbound_loopback_curl,
    imap_fetch_only_inbox_message,
    provision_mail_recipient,
    register_primary_domain,
)

# Reuse the inbound round-trip harness fixtures (image build, scanner sidecars,
# claimed plaintext nest with the four mail ports mapped) + its claimed domain —
# priority #2 (reuse, don't duplicate the ~70 lines of fixture boilerplate).
# Storage mode is irrelevant to greylisting, so the plaintext round_trip_nest is
# the right reuse.
from .test_mail_deploy_inbound_round_trip import (  # noqa: F401  (fixtures used by name)
    DOMAIN,
    docker_image,
    round_trip_nest,
    scanners,
)

pytestmark = [pytest.mark.tier_4]

_RECIPIENT_LOCAL = "greylist-user"
_RECIPIENT_PASSWORD = "greylist-retry-password-1"
_GREYLIST_DELAY_SECS = 2


def _enable_greylist_only(nest) -> None:
    """Spam policy with ONLY the greylist gate on (short delay). Mirrors
    `relax_spam_policy` but flips `greylist_enabled` so the inbound is deferred on
    first-seen; the other gates stay relaxed so the synthetic loopback peer isn't
    rejected for an unrelated reason. Projected via fetch_config — set before enable."""
    with admin_ws(nest) as admin:
        admin.call(
            "fauna.bridges.put_spam_policy",
            {
                "baseline_standing_publish": False,
                "dnsbl_servers": [],
                "greylist_enabled": True,
                "greylist_delay_secs": _GREYLIST_DELAY_SECS,
                "fcrdns_mode": "off",
                "max_conn_per_min": 1000,
            },
        )


@pytest.mark.feature("mail-server")
def test_mail_deploy_greylist_then_retry_delivered(round_trip_nest, run_seal_helper):
    nest = round_trip_nest
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port

    register_primary_domain(nest, DOMAIN)
    _enable_greylist_only(nest)
    recipient = provision_mail_recipient(
        nest, run_seal_helper, domain=DOMAIN, local_part=_RECIPIENT_LOCAL,
        password=_RECIPIENT_PASSWORD,
    )
    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    mail_from = "sender@host.docker.internal"  # resolvable in-container (sender-domain check)
    subject = "Gap-2 greylist retry round-trip"
    body_marker = "deploy-image greylist retry"
    body_text = f"Hello from the {body_marker} test.\n"

    # ── First attempt: first-seen (peer, from, to) triplet → 451 deferral at RCPT,
    #    NOT stored. Use the non-raising twin so we can assert the tempfail. ──
    rc, out = deliver_inbound_attempt_loopback_curl(
        name, mail_from=mail_from, rcpt_to=recipient["username"],
        subject=subject, body_text=body_text,
    )
    assert rc != 0, (
        "first inbound from a never-seen triplet must be greylist-deferred, not "
        f"accepted; curl exited 0 (accepted). output: {out!r}\n"
        f"── bridge diagnostics ──\n{bridge_diag(name)}"
    )
    assert "451" in out, (
        "first inbound must be deferred with 451 4.7.1 (Greylisted; try again "
        f"later); curl output carried no 451: {out!r}"
    )

    # ── Retry the SAME triplet after the delay window. Greylist keys on (peer,
    #    MAIL FROM, RCPT TO), not message content, so a fresh Message-ID is fine;
    #    the loopback peer + same envelope make the triplet match. Now past the
    #    2 s delay → accepted (250). deliver_inbound_loopback_curl raises on
    #    non-250, so a successful return IS the accept assertion. ──
    time.sleep(_GREYLIST_DELAY_SECS + 3)
    try:
        msg_id = deliver_inbound_loopback_curl(
            name, mail_from=mail_from, rcpt_to=recipient["username"],
            subject=subject, body_text=body_text,
        )
    except RuntimeError as e:
        raise AssertionError(
            f"retry past the greylist delay must be ACCEPTED, but delivery failed: {e}\n"
            f"── bridge diagnostics ──\n{bridge_diag(name)}"
        ) from e

    # ── Read it back decrypted. imap_fetch_only_inbox_message asserts exactly ONE
    #    INBOX message — so it also proves the greylisted first attempt left no
    #    stored copy. ──
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", mp[993], DOMAIN,
            recipient["username"], recipient["password"],
        )
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(name)}") from e

    text = raw.decode("utf-8", errors="replace")
    assert subject in text, f"decrypted body must carry the Subject; got: {text[:400]!r}"
    assert body_marker in text, f"decrypted body must carry the message text; got: {text[:400]!r}"
    assert msg_id.strip("<>") in text, "decrypted body must carry the retry's Message-ID"
