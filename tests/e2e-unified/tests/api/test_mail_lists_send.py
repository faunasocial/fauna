"""tier_3 e2e for the mass-mailing list-SEND pipeline (tracked internally, item #12).

Drives the User-class list RPC surface (`fauna.bridges.{create,list,update}_account_list`,
`add_list_member`, `unsubscribe_list_member`, `resubscribe_list_member`,
`list_list_members`, `send_list_message`) + the Admin `rotate_list_unsubscribe_secret`
over the real WS-RPC socket against a real `fauna-nest` binary, with a real
`fauna-mail-bridge` MTA delivering the fan-out to an in-process stub external MX
(`helpers/stub_mx.py`). The `mail_bridge_mta` fixture claims the primary
local domain `fauna.test` (the nest holds its DKIM key) and routes `external.test` (the list
members' domain) at the stub MX, so list mail leaves over the real outbound path.

What this proves that the unit tests can't:

- **Option C delivery-time DKIM signing end-to-end.** `send_list_message` stamps
  each recipient's RFC 8058 `List-*` headers nest-side, and the nest signs every
  outbound row at the hand-out (`fauna.bridges.fetch_outbound_due`), after the
  stamping. `test_list_send_stamps_rfc8058_headers` captures the delivered
  bytes and has an independent verifier (dkimpy) confirm the signature is valid
  AND covers the `List-*` set — the only test that exercises the full
  nest-stamps → nest-signs-at-hand-out → bridge-delivers chain.
- **The one-click unsubscribe loop, both channels** — the HTTPS `POST /list/unsubscribe`
  and the SMTP `unsubscribe+<token>@` mailto, each driven with a token extracted
  from a *delivered* `List-Unsubscribe` header (no test-only secret injection).
- **The per-list rate caps** — the per-send hard reject (`552`) via a real
  per-list `recipients_per_send` override, and the per-account-per-day tempfail
  (`452`) via a lowered deployment ceiling (the `mass_mailing_test_hook`).

- **A list send spends none of the owner's everyday allowance.**
  `test_list_per_actor_rate_cap_not_consumed_by_list_send` reads the per-actor
  submission counter at rest before and after a list send made with it spent.

Deferred (see TODO #12 close-out notes): `test_list_admin_sees_count_not_members`
(no cross-user admin list RPC exists in this track — it lands with the flat
`admin-mail` page).
"""

import json
import re
import secrets
import time
import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _poke_outbound_bridge,
    _wait_for_tagged,
    dkim_signature_tag,
)
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# How long to wait for a list message to reach the stub external MX. The bridge's
# outbound worker polls nest's `outbound_mail_queue` on a **30 s** `PollInterval`
# (`internal/mta/outbound.go`). The nest nudges it sooner (`outbound_ready`) from
# the interactive `fauna.email.send` path (`smtp-server.md` § Outbound delivery →
# Prompt-drain nudge); a `send_list_message` fan-out emits no nudge and rides the
# poll backstop, so list rows deliver on the next poll: up to ~30 s after
# `send_list_message` returns. 60 s clears one full poll cycle with margin on a
# loaded shared box. `_wait_for_tagged` returns the instant the message arrives,
# so this is a ceiling, not a fixed sleep — do NOT lower it below the 30 s poll
# interval or the captures become timing-flaky. A test that follows other
# outbound traffic on the shared bridge pokes the drain instead and waits one
# `MAIL_OUTBOUND_CYCLE_S` (the worker may still be inside an earlier row's MX
# attempt when the poll comes due).
_DELIVERY_TIMEOUT = 60.0


# ── client / actor helpers ───────────────────────────────────────────


def _fresh_user(nest_instance):
    """A fresh registered (User-class) actor on the shared nest — the list owner."""
    return create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _admin_client(nest_instance):
    """An Admin-class WS-RPC client signing as the deployment admin (for
    `rotate_list_unsubscribe_secret`, the lone admin-class list kind)."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


# ── list / member RPC helpers ────────────────────────────────────────


def _create_list(client, domain, *, friendly_name=None, recipients_per_send=None):
    """Create a list on `domain` with a random local-part; return (list_id, local_part)."""
    local_part = "news-" + secrets.token_hex(4)
    payload = {"local_part": local_part, "local_domain": domain}
    if friendly_name is not None:
        payload["friendly_name"] = friendly_name
    if recipients_per_send is not None:
        payload["recipients_per_send"] = recipients_per_send
    list_id = client.call("fauna.bridges.create_account_list", payload)["list_id"]
    return list_id, local_part


def _add_member(client, list_id, address):
    return client.call(
        "fauna.bridges.add_list_member",
        {"list_id": list_id, "recipient_address": address},
    )


def _members(client, list_id, include_unsubscribed=True):
    return client.call(
        "fauna.bridges.list_list_members",
        {"list_id": list_id, "include_unsubscribed": include_unsubscribed},
    )


def _send(client, list_id, message: bytes):
    return client.call(
        "fauna.bridges.send_list_message",
        {"list_id": list_id, "message": message},
    )


def _external_addr():
    """A fresh external (non-local-domain) member address routed at the stub MX."""
    return f"sub-{secrets.token_hex(4)}@external.test"


# ── message + capture helpers ────────────────────────────────────────


def _compose(from_addr, to_addr, subject, domain):
    """A minimal RFC 5322 message the list owner composes. The `From:` is on a
    local mail domain, so the nest holds a key to sign with at the hand-out."""
    lines = [
        f"From: {from_addr}",
        f"To: {to_addr}",
        f"Subject: {subject}",
        f"Message-ID: <{secrets.token_hex(6)}@{domain}>",
        "Date: Sun, 14 Jun 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "This is a Fauna mailing-list test message.",
        "",
    ]
    return ("\r\n".join(lines) + "\r\n").encode()


def _tagged(stub_mx, subject_token):
    """Every captured stub-MX message carrying `subject_token` in its raw bytes."""
    needle = subject_token.encode()
    return [m for m in stub_mx.messages() if needle in m]


def _extract_unsub_token(raw: bytes) -> str:
    """Pull the one-click token from a delivered `List-Unsubscribe` header. The
    https form is `<https://<primary>/list/unsubscribe?t=<token>>`; the token is
    base64url (no `+`), so a raw-bytes search is fold-robust."""
    m = re.search(rb"/list/unsubscribe\?t=([A-Za-z0-9_\-]+)", raw)
    assert m, f"no List-Unsubscribe https token in captured message:\n{raw[:600]!r}"
    return m.group(1).decode()


# ── HTTPS one-click + test-hook helpers ──────────────────────────────


def _post_unsubscribe(nest_url: str, token: str) -> int:
    """`POST /list/unsubscribe?t=<token>` with the RFC 8058 One-Click body.
    Returns the HTTP status (200 unsubscribed/already, 404 unknown/rotated-out)."""
    req = urllib.request.Request(
        f"{nest_url}/list/unsubscribe?t={token}",
        data=b"List-Unsubscribe=One-Click",
        headers={"Content-Type": "application/x-www-form-urlencoded"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=5.0) as resp:
            return resp.status
    except urllib.error.HTTPError as e:
        return e.code


def _set_mass_mailing_policy(nest_url: str, **overrides) -> None:
    """Set (or, with no kwargs, reset to catalog defaults) the mass-mailing policy
    overrides via the `--features test-hooks` endpoint. Fields:
    `list_recipients_per_send_ceiling`, `list_recipients_per_account_per_day_ceiling`,
    `list_recipients_per_deployment_per_day_ceiling`, `list_max_import_per_batch`."""
    body = json.dumps(overrides).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/mass-mailing/policy",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"mass-mailing policy hook returned {resp.status}"


# ── tests ─────────────────────────────────────────────────────────────


def test_create_list_persists_alias_and_list_rows(nest_instance, mail_bridge_mta):
    """create_account_list persists the sixth `kind='list'` alias + the
    `mail_lists` row, surfaces it in list_account_lists, and EXCLUDES it from
    the personal-alias surface (lists are managed on the separate page)."""
    domain = mail_bridge_mta.domain
    user = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain, friendly_name="Weekly")
        assert isinstance(list_id, bytes) and len(list_id) == 16

        rows = client.call("fauna.bridges.list_account_lists", {})["lists"]
        row = next(r for r in rows if r["list_id"] == list_id)
        assert row["pattern"] == local_part
        assert row["local_domain"] == domain
        assert row["friendly_name"] == "Weekly"
        assert row["member_count"] == 0
        assert row["owner_actor_id"] == user["actor_id_bytes"]

        # The list's `kind='list'` alias is NOT a personal alias.
        aliases = client.call("fauna.bridges.list_account_aliases", {})["aliases"]
        assert all(a["pattern"] != local_part for a in aliases), (
            "the list's kind='list' alias leaked into the personal-alias surface"
        )


@pytest.mark.feature("mailing-lists")
def test_list_send_stamps_rfc8058_headers(nest_instance, mail_bridge_mta):
    """THE Option C proof: send_list_message stamps the per-recipient RFC 8058 /
    RFC 2369 headers nest-side and the nest DKIM-signs the row at the hand-out.

    Captures the delivered bytes at the stub MX and asserts (a) the List-* headers
    are present with the spec values, (b) a DKIM-Signature exists whose `h=` set
    covers the List-* names, and (c) an independent verifier (dkimpy) confirms the
    signature is cryptographically valid AND aligned to the From domain — the same
    `dkim=pass` bar a real receiver (Gmail) applies, now over the list-send path.
    """
    import dkim  # dkimpy — independent verifier, fleet venv dep

    handle = mail_bridge_mta
    domain = handle.domain
    assert handle.dkim_public_dns_value, (
        "fixture read no DKIM record — the list-signing proof cannot run"
    )
    user = _fresh_user(nest_instance)
    token = f"listdkim-{int(time.time() * 1000)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain, friendly_name="Bob's Weekly")
        list_addr = f"{local_part}@{domain}"
        member = _external_addr()
        assert _add_member(client, list_id, member)["added"] is True

        msg = _compose(f"Bob's Weekly <{list_addr}>", list_addr, f"Issue {token}", domain)
        reply = _send(client, list_id, msg)
        assert reply["queued_count"] == 1

    received = _wait_for_tagged(handle.stub_mx, token, timeout=_DELIVERY_TIMEOUT)
    assert received is not None, (
        f"stub external MX received no list message tagged {token} within 20s — "
        f"the fan-out / delivery did not complete (bridge log: {handle.log_file})"
    )

    # (a) the authoritative List-* set, stamped by the nest.
    low = received.lower()
    assert b"list-unsubscribe-post: list-unsubscribe=one-click" in low
    assert b"precedence: bulk" in low
    assert b"list-id:" in low
    assert b"list-unsubscribe:" in low
    assert (b"<mailto:unsubscribe+" in low) and (b"/list/unsubscribe?t=" in low), (
        "List-Unsubscribe lacks the mailto + https one-click pair"
    )

    # (b) a delivery-time DKIM-Signature whose signed-header set covers List-*.
    h_tag = dkim_signature_tag(received, "h")
    assert h_tag is not None, (
        "no DKIM-Signature on the delivered list mail — the nest did not sign "
        "the list row at the fetch_outbound_due hand-out"
    )
    h_low = h_tag.lower()
    for name in ("list-id", "list-unsubscribe", "list-unsubscribe-post"):
        assert name in h_low, f"{name} missing from the DKIM h= set: {h_tag!r}"

    # (c) the signature cryptographically verifies + is From-aligned (DMARC).
    expected_query = f"{handle.dkim_selector}._domainkey.{domain}.".encode()
    seen: list[bytes] = []

    def dnsfunc(name, timeout=5):
        seen.append(name)
        return handle.dkim_public_dns_value if name == expected_query else None

    assert dkim.verify(received, dnsfunc=dnsfunc), (
        "dkimpy rejected the list mail's DKIM-Signature — it would fail dkim=pass "
        f"at a real receiver. dnsfunc queries: {seen!r}. First 700 bytes:\n{received[:700]!r}"
    )
    assert dkim_signature_tag(received, "d") == domain, (
        "DKIM d= is not aligned to the From: list domain — DMARC would fail"
    )


@pytest.mark.feature("mailing-lists")
def test_list_unsubscribed_member_excluded_from_send(nest_instance, mail_bridge_mta):
    """Subscribe 3, unsubscribe 1, send → exactly 2 messages leave (the nest
    fans out only to subscribed members)."""
    handle = mail_bridge_mta
    domain = handle.domain
    user = _fresh_user(nest_instance)
    token = f"listexcl-{int(time.time() * 1000)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain)
        list_addr = f"{local_part}@{domain}"
        members = [_external_addr() for _ in range(3)]
        for m in members:
            _add_member(client, list_id, m)
        client.call(
            "fauna.bridges.unsubscribe_list_member",
            {"list_id": list_id, "recipient_address": members[0]},
        )

        reply = _send(client, list_id, _compose(list_addr, list_addr, token, domain))
        assert reply["queued_count"] == 2, "fan-out should skip the unsubscribed member"

    # The first delivery proves the path is live; then the count is authoritative.
    # Both rows are delivered in one poll batch (the worker drains a non-empty
    # batch then re-polls immediately), so the second lands within ~1 s of the
    # first — a short post-arrival drain window suffices.
    assert _wait_for_tagged(handle.stub_mx, token, timeout=_DELIVERY_TIMEOUT) is not None
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and len(_tagged(handle.stub_mx, token)) < 2:
        time.sleep(0.25)
    assert len(_tagged(handle.stub_mx, token)) == 2, (
        "exactly 2 of 3 members should have received the message"
    )


@pytest.mark.feature("mailing-lists")
def test_list_one_click_unsubscribe_via_https(nest_instance, mail_bridge_mta):
    """A delivered List-Unsubscribe https token POSTed One-Click flips the member;
    a second POST is idempotent (200 'already')."""
    handle = mail_bridge_mta
    domain = handle.domain
    user = _fresh_user(nest_instance)
    token = f"listhttps-{int(time.time() * 1000)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain)
        list_addr = f"{local_part}@{domain}"
        member = _external_addr()
        _add_member(client, list_id, member)
        _send(client, list_id, _compose(list_addr, list_addr, token, domain))

        received = _wait_for_tagged(handle.stub_mx, token, timeout=_DELIVERY_TIMEOUT)
        assert received is not None
        unsub_token = _extract_unsub_token(received)

        assert _post_unsubscribe(handle.nest_url, unsub_token) == 200
        m = next(r for r in _members(client, list_id)["members"] if r["recipient_address"] == member)
        assert m["unsubscribed_at"] is not None, "the member was not unsubscribed"

        # Idempotent: a second One-Click POST is still 200.
        assert _post_unsubscribe(handle.nest_url, unsub_token) == 200


@pytest.mark.feature("mailing-lists")
def test_list_one_click_unsubscribe_via_mailto(nest_instance, mail_bridge_mta):
    """A delivered List-Unsubscribe mailto token, sent to `unsubscribe+<token>@<domain>`
    over SMTP, is accepted (RCPT 250, body discarded at DATA) and flips the member.

    Exercises the #5 nest `resolve_recipient` → `ResolveRecipientReply::Discard`
    path through the real Go MTA."""
    handle = mail_bridge_mta
    domain = handle.domain
    user = _fresh_user(nest_instance)
    token = f"listmailto-{int(time.time() * 1000)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain)
        list_addr = f"{local_part}@{domain}"
        member = _external_addr()
        _add_member(client, list_id, member)
        _send(client, list_id, _compose(list_addr, list_addr, token, domain))

        received = _wait_for_tagged(handle.stub_mx, token, timeout=_DELIVERY_TIMEOUT)
        assert received is not None
        unsub_token = _extract_unsub_token(received)

        # Send the mailto one-click to port 25. The Discard outcome accepts the
        # RCPT (250) and drops the body at DATA — still a 250 on `.`.
        unsub_rcpt = f"unsubscribe+{unsub_token}@{domain}"
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
            conn.cmd("MAIL FROM:<mua@external.test>", "250", deadline)
            conn.cmd(f"RCPT TO:<{unsub_rcpt}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(b"List-Unsubscribe=One-Click\r\n")
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

        # The flip is fire-and-forget at RCPT-resolution time — poll briefly.
        poll = time.monotonic() + 10.0
        flipped = False
        while time.monotonic() < poll:
            m = next(r for r in _members(client, list_id)["members"] if r["recipient_address"] == member)
            if m["unsubscribed_at"] is not None:
                flipped = True
                break
            time.sleep(0.25)
        assert flipped, "mailto one-click did not unsubscribe the member"


def _submission_used(db_path, actor_id, day_bucket, set_to=None):
    """The owner's per-actor daily submission counter (`bridge_submission_quota`,
    the one pool the mail-app submission path and `fauna.email.send` draw from).
    `set_to` pre-spends it — a fixture poke standing in for a day of sending."""
    import sqlite3

    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        if set_to is not None:
            conn.execute(
                "INSERT INTO bridge_submission_quota (actor_id, day_bucket, used) "
                "VALUES (?1, ?2, ?3) ON CONFLICT(actor_id, day_bucket) DO UPDATE SET "
                "used = excluded.used", (actor_id, day_bucket, set_to))
            conn.commit()
        row = conn.execute(
            "SELECT used FROM bridge_submission_quota WHERE actor_id = ? AND day_bucket = ?",
            (actor_id, day_bucket)).fetchone()
        return row[0] if row else 0
    finally:
        conn.close()


@pytest.mark.feature("mailing-lists")
def test_list_per_actor_rate_cap_not_consumed_by_list_send(nest_instance, mail_bridge_mta):
    """A newsletter does not use the owner's everyday sending allowance: with
    that allowance already spent for the day, the list send still goes out to
    every member, and the owner's counter does not move (`mail-mass-mailing.md`
    § Goal — list sends draw on the list quotas alone)."""
    handle = mail_bridge_mta
    domain = handle.domain
    user = _fresh_user(nest_instance)
    db = nest_instance["db_path"]
    bucket = int(time.time()) // 86400
    spent = 1000  # the catalog default `mail.submission.max_per_day`
    token = f"listquota-{secrets.token_hex(4)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain)
        list_addr = f"{local_part}@{domain}"
        for _ in range(3):
            _add_member(client, list_id, _external_addr())
        assert _submission_used(db, user["actor_id_bytes"], bucket, set_to=spent) == spent
        reply = _send(client, list_id, _compose(list_addr, list_addr, token, domain))
        assert reply["queued_count"] == 3, (
            f"a spent everyday allowance must not hold back a list send; got {reply!r}")
    # Run a drain cycle now rather than waiting out the poll: the invariant is
    # that the send goes out at all, not how soon the backstop comes round.
    _poke_outbound_bridge(nest_instance["url"])
    assert _wait_for_tagged(handle.stub_mx, token, timeout=MAIL_OUTBOUND_CYCLE_S) is not None, (
        "the list send must actually go out")
    assert _submission_used(db, user["actor_id_bytes"], bucket) == spent, (
        "a list send must leave the owner's everyday submission counter untouched")


@pytest.mark.feature("mailing-lists")
def test_list_per_send_cap_rejects_oversize(nest_instance, mail_bridge_mta):
    """A per-list `recipients_per_send` override (a real User RPC, ≤ the admin
    ceiling) hard-rejects an over-cap send (`552 5.3.4`, no auto-chunking)."""
    domain = mail_bridge_mta.domain
    user = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain, recipients_per_send=2)
        list_addr = f"{local_part}@{domain}"
        for _ in range(3):
            _add_member(client, list_id, _external_addr())

        with pytest.raises(RpcCallError) as e:
            _send(client, list_id, _compose(list_addr, list_addr, "oversize", domain))
        assert e.value.code == "fauna.bridges.list_per_send_cap_exceeded"


@pytest.mark.feature("mailing-lists")
def test_list_resub_undoes_unsubscribe(nest_instance, mail_bridge_mta):
    """A resubscribed member receives the next send."""
    handle = mail_bridge_mta
    domain = handle.domain
    user = _fresh_user(nest_instance)
    token = f"listresub-{int(time.time() * 1000)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain)
        list_addr = f"{local_part}@{domain}"
        member = _external_addr()
        _add_member(client, list_id, member)
        client.call(
            "fauna.bridges.unsubscribe_list_member",
            {"list_id": list_id, "recipient_address": member},
        )
        client.call(
            "fauna.bridges.resubscribe_list_member",
            {"list_id": list_id, "recipient_address": member},
        )
        reply = _send(client, list_id, _compose(list_addr, list_addr, token, domain))
        assert reply["queued_count"] == 1

    assert _wait_for_tagged(handle.stub_mx, token, timeout=_DELIVERY_TIMEOUT) is not None


@pytest.mark.feature("mailing-lists", "admin-mail-policy")
def test_list_token_deterministic_across_secret_rotation(nest_instance, mail_bridge_mta):
    """Rotating the deployment unsubscribe secret re-tokenizes every member: the
    new send carries a different token, the old token's POST now 404s, the new
    one 200s."""
    handle = mail_bridge_mta
    domain = handle.domain
    user = _fresh_user(nest_instance)
    tok1 = f"listrot1-{int(time.time() * 1000)}"
    tok2 = f"listrot2-{int(time.time() * 1000)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain)
        list_addr = f"{local_part}@{domain}"
        _add_member(client, list_id, _external_addr())

        _send(client, list_id, _compose(list_addr, list_addr, tok1, domain))
        first = _wait_for_tagged(handle.stub_mx, tok1, timeout=_DELIVERY_TIMEOUT)
        assert first is not None
        unsub1 = _extract_unsub_token(first)

        # Rotate the deployment-wide secret (Admin-class RPC).
        with _admin_client(nest_instance) as admin:
            rotated = admin.call("fauna.bridges.rotate_list_unsubscribe_secret", {})
            assert rotated["members_retokenized"] >= 1

        _send(client, list_id, _compose(list_addr, list_addr, tok2, domain))
        second = _wait_for_tagged(handle.stub_mx, tok2, timeout=_DELIVERY_TIMEOUT)
        assert second is not None
        unsub2 = _extract_unsub_token(second)

    assert unsub1 != unsub2, "rotation must change the one-click token"
    # The old token is no longer in the index → 404; the new one flips → 200.
    assert _post_unsubscribe(handle.nest_url, unsub1) == 404
    assert _post_unsubscribe(handle.nest_url, unsub2) == 200


@pytest.mark.feature("mailing-lists")
def test_list_per_day_cap_tempfails(nest_instance, mail_bridge_mta):
    """Lowering the per-account-per-day list-recipient ceiling makes an over-cap
    send tempfail (`452 4.7.0`, `fauna.bridges.list_daily_cap_exceeded`) — the
    four-scope rate accounting (#7/#8) enforced end-to-end through the handler.

    Uses the `mass_mailing_test_hook` to set the deployment ceiling (its admin
    write RPC lands with the flat `admin-mail` page); resets it in a finally so
    other tests on the session-scoped nest see the catalog default."""
    domain = mail_bridge_mta.domain
    nest_url = mail_bridge_mta.nest_url
    user = _fresh_user(nest_instance)
    try:
        _set_mass_mailing_policy(nest_url, list_recipients_per_account_per_day_ceiling=1)
        with _user_client(nest_instance, user) as client:
            list_id, local_part = _create_list(client, domain)
            list_addr = f"{local_part}@{domain}"
            # Two subscribed members > the ceiling of 1 → per-account-day tempfail.
            _add_member(client, list_id, _external_addr())
            _add_member(client, list_id, _external_addr())

            with pytest.raises(RpcCallError) as e:
                _send(client, list_id, _compose(list_addr, list_addr, "overday", domain))
            assert e.value.code == "fauna.bridges.list_daily_cap_exceeded"
    finally:
        # Reset to catalog defaults (all None) so the lowered ceiling can't bleed
        # into other list tests sharing this session-scoped nest.
        _set_mass_mailing_policy(nest_url)


# ── refusals and sticky state (the read-back's nest outcomes) ───────────────


@pytest.mark.feature("mailing-lists")
def test_list_member_on_local_domain_refused_pointing_at_aliases(nest_instance, mail_bridge_mta):
    """A member address on a domain this nest hosts is refused
    `recipient_on_local_domain`, and the sentence every app renders for that code
    points at aliases, the remedy (`mail-mass-mailing.md` § Don't do these)."""
    domain = mail_bridge_mta.domain
    user = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        list_id, _ = _create_list(client, domain)
        with pytest.raises(RpcCallError) as e:
            _add_member(client, list_id, f"neighbour-{secrets.token_hex(3)}@{domain}")
        assert e.value.code == "fauna.bridges.recipient_on_local_domain", e.value.code
        assert _members(client, list_id)["members"] == []
    assert "alias" in S.error.bridges.recipient_on_local_domain.lower()


@pytest.mark.feature("mailing-lists")
def test_list_address_taken_or_reserved_refused(nest_instance, mail_bridge_mta):
    """A list cannot take an address someone already holds, nor a reserved one
    (`unsubscribe`, which the mailto handler owns, or a role name such as
    `postmaster`) — § Reserved local-part: `unsubscribe@`."""
    domain = mail_bridge_mta.domain
    user = _fresh_user(nest_instance)
    taken = "held-" + secrets.token_hex(4)
    with _user_client(nest_instance, user) as client:
        client.call("fauna.bridges.create_account_alias", {
            "kind": "exact", "local_domain": domain, "pattern": taken,
            "controls": {"label": ""},
        })
        with pytest.raises(RpcCallError) as e:
            client.call("fauna.bridges.create_account_list",
                        {"local_part": taken, "local_domain": domain})
        assert e.value.code == "fauna.bridges.conflicts_with_existing_alias", e.value.code
        for reserved in ("unsubscribe", "postmaster"):
            with pytest.raises(RpcCallError) as e:
                client.call("fauna.bridges.create_account_list",
                            {"local_part": reserved, "local_domain": domain})
            assert e.value.code == "fauna.bridges.reserved_local_part", (reserved, e.value.code)
        assert client.call("fauna.bridges.list_account_lists", {})["lists"] == []


@pytest.mark.feature("mailing-lists")
def test_bare_unsubscribe_address_refused_with_explanation(nest_instance, mail_bridge_mta):
    """Mail to bare `unsubscribe@` (no token) is refused at RCPT with a reply
    that says a list-unsubscribe token is needed (§ Reserved local-part)."""
    handle = mail_bridge_mta
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<confused@external.test>", "250", deadline)
        reply = conn.cmd(f"RCPT TO:<unsubscribe@{handle.domain}>", "550", deadline)
        assert "token" in reply.lower(), f"the refusal must explain itself; got {reply!r}"
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mailing-lists")
def test_mail_to_a_list_address_is_refused(nest_instance, mail_bridge_mta):
    """A peer mailing a list's own address is refused at RCPT with the goal's
    `550 5.1.1 List submissions not accepted at this address` — a list only
    sends (§ Pattern)."""
    handle = mail_bridge_mta
    user = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        _, local_part = _create_list(client, handle.domain)
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<reader@external.test>", "250", deadline)
        reply = conn.cmd(f"RCPT TO:<{local_part}@{handle.domain}>", "550", deadline)
        assert "5.1.1 List submissions not accepted at this address" in reply, (
            f"the list-address refusal must carry the goal's text; got {reply!r}"
        )
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mailing-lists")
def test_unsubscribe_is_sticky_through_readd_and_import(nest_instance, mail_bridge_mta):
    """A member who unsubscribed stays unsubscribed when the owner adds or
    imports them again; only an explicit re-subscribe brings them back
    (§ Architectural rules)."""
    domain = mail_bridge_mta.domain
    user = _fresh_user(nest_instance)
    member = _external_addr()
    with _user_client(nest_instance, user) as client:
        list_id, _ = _create_list(client, domain)
        _add_member(client, list_id, member)
        client.call("fauna.bridges.unsubscribe_list_member",
                    {"list_id": list_id, "recipient_address": member})

        def state():
            rows = [r for r in _members(client, list_id)["members"]
                    if r["recipient_address"] == member]
            assert len(rows) == 1, rows
            return rows[0]["unsubscribed_at"]

        assert state() is not None
        try:
            _add_member(client, list_id, member)
        except RpcCallError:
            pass  # a refusal is also sticky; the state below is what matters
        assert state() is not None, "re-adding must not re-subscribe"
        client.call("fauna.bridges.batch_import_list_members",
                    {"list_id": list_id, "addresses": [member]})
        assert state() is not None, "re-importing must not re-subscribe"
        client.call("fauna.bridges.resubscribe_list_member",
                    {"list_id": list_id, "recipient_address": member})
        assert state() is None, "re-subscribe is the one way back"


def _get_unsubscribe_page(nest_url: str, token: str):
    with urllib.request.urlopen(f"{nest_url}/list/unsubscribe?t={token}", timeout=5.0) as r:
        return r.status, r.read().decode("utf-8", "replace")


@pytest.mark.feature("mailing-lists")
def test_list_unsubscribe_get_is_read_only_and_expired_link_says_so(nest_instance, mail_bridge_mta):
    """Opening a delivered unsubscribe link in a browser (GET) shows a confirm
    button and changes nothing; confirming (the POST) unsubscribes. A link whose
    token the nest no longer knows is answered with a page saying it expired
    (§ The HTTPS endpoint; § Secret rotation)."""
    handle = mail_bridge_mta
    domain = handle.domain
    user = _fresh_user(nest_instance)
    tag = f"listget-{secrets.token_hex(4)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain)
        list_addr = f"{local_part}@{domain}"
        member = _external_addr()
        _add_member(client, list_id, member)
        _send(client, list_id, _compose(list_addr, list_addr, tag, domain))
        received = _wait_for_tagged(handle.stub_mx, tag, timeout=_DELIVERY_TIMEOUT)
        assert received is not None, "the list send must reach the stub MX"
        token = _extract_unsub_token(received)

        status, page = _get_unsubscribe_page(handle.nest_url, token)
        assert status == 200 and "Confirm unsubscribe" in page, page[:400]
        m = next(r for r in _members(client, list_id)["members"]
                 if r["recipient_address"] == member)
        assert m["unsubscribed_at"] is None, "a GET must never unsubscribe"

        assert _post_unsubscribe(handle.nest_url, token) == 200
        m = next(r for r in _members(client, list_id)["members"]
                 if r["recipient_address"] == member)
        assert m["unsubscribed_at"] is not None, "the confirm POST unsubscribes"

    unknown = "x" + secrets.token_urlsafe(24).replace("-", "a").replace("_", "b")
    req = urllib.request.Request(
        f"{handle.nest_url}/list/unsubscribe?t={unknown}",
        data=b"List-Unsubscribe=One-Click",
        headers={"Content-Type": "application/x-www-form-urlencoded"}, method="POST")
    with pytest.raises(urllib.error.HTTPError) as e:
        urllib.request.urlopen(req, timeout=5.0)
    assert e.value.code == 404
    assert "expired" in e.value.read().decode("utf-8", "replace").lower()


@pytest.mark.feature("mailing-lists")
def test_list_help_link_opens_a_subscribe_unsubscribe_page(nest_instance, mail_bridge_mta):
    """The `List-Help` header on a delivered issue names `/list/<list-id>/help`,
    and that page explains how to subscribe and unsubscribe (§ RFC 2369 list
    headers). The header's host is the deployment's public domain, which the
    test box does not resolve, so the page is fetched at the header's path on
    the nest's own HTTP endpoint."""
    handle = mail_bridge_mta
    domain = handle.domain
    user = _fresh_user(nest_instance)
    tag = f"listhelp-{secrets.token_hex(4)}"
    with _user_client(nest_instance, user) as client:
        list_id, local_part = _create_list(client, domain)
        list_addr = f"{local_part}@{domain}"
        _add_member(client, list_id, _external_addr())
        _send(client, list_id, _compose(list_addr, list_addr, tag, domain))
    received = _wait_for_tagged(handle.stub_mx, tag, timeout=_DELIVERY_TIMEOUT)
    assert received is not None, "the list send must reach the stub MX"

    m = re.search(rb"(?im)^List-Help:\s*<https://[^/>]+(/list/[^/>]+/help)>", received)
    assert m, f"no default List-Help header on the delivered issue: {received[:700]!r}"
    path = m.group(1).decode()
    with urllib.request.urlopen(f"{handle.nest_url}{path}", timeout=5.0) as r:
        status, page = r.status, r.read().decode("utf-8", "replace")
    assert status == 200, page[:400]
    lower = page.lower()
    assert "to unsubscribe" in lower and "to subscribe" in lower, page[:600]
