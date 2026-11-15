"""DMARC aggregate reports mailed to the nest are processed, not delivered.

`docs/goal/behavior/dmarc-reporting.md` § Aggregate-report receive (RUA): every
domain the nest serves publishes `rua=mailto:dmarc-report@<primary>`, so large
receivers mail it a gzipped XML report a day. The MTA routes `dmarc-report@` at
RCPT TO to the nest's processor (`resolve_recipient` → `Processor`), which
validates and stores the report; the admin lists it over the wire. A malformed
report, or one about a domain the nest does not serve, is accepted on the wire
(250) and dropped. None of them ever reaches a mailbox.

Real SMTP into the real MTA beside a real nest, on a mail venue of this
module's own: nobody else mails this nest, so its inbound heartbeat
(`fauna.bridges.mail_health` → `last_inbound_accepted_at`, stamped by every
mailbox ingest) staying unset is the observation that no copy reached the
admin's inbox.

Test taxonomy: `tier_3` — every binary real, real wire end to end; no app
driver participates.
"""

import base64
import contextlib
import gzip
import time
import uuid

import pytest

from helpers.fixture_closure import MAIL_VENUE_BINARIES, resolves_lazily
from helpers.dmarc_reports import mail_report, report_message, report_xml
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _open_mail_gate_and_wait_for_the_mta,
    _wait_for_tagged,
)

pytestmark = pytest.mark.tier_3


def _admin_ws(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


@pytest.fixture(scope="module")
@resolves_lazily(*MAIL_VENUE_BINARIES)
def dmarc_venue(request, nest_mode, tmp_path_factory):
    """A claimed mail nest of this module's own, its mail gate open."""
    from conftest import _start_mail_venue

    handle, cleanup = _start_mail_venue(
        request, nest_mode, tmp_path_factory, "dmarc-report-venue"
    )
    try:
        _open_mail_gate_and_wait_for_the_mta(handle)
        yield handle
    finally:
        cleanup()


# The report, its attachment and the SMTP transaction are shared with the admin
# page's witness (`test_admin_mail_reports.py`).
_report_xml = report_xml
_report_message = report_message
_mail = mail_report


@pytest.mark.feature("dmarc-reporting")
def test_dmarc_report_is_stored_and_listed_never_delivered(dmarc_venue):
    """A valid report is stored and listed to the admin; a malformed one and
    one about a foreign domain are accepted on the wire but not stored; no copy
    of any of them reaches a mailbox."""
    handle = dmarc_venue
    domain = handle.domain
    rcpt = f"dmarc-report@{domain}"
    stamp = int(time.time() * 1000)
    good_id = f"good-{stamp}"

    _mail(handle, rcpt, _report_message(rcpt, _report_xml(domain, good_id), good_id))
    # Required field missing (`report_id`): malformed → 250, dropped.
    _mail(handle, rcpt, _report_message(rcpt, _report_xml(domain, ""), f"bad-{stamp}"))
    # A DOCTYPE is refused outright (no entity is ever expanded).
    doctype = _report_xml(domain, f"dtd-{stamp}").replace(
        "<feedback", '<!DOCTYPE feedback [<!ENTITY x "x">]><feedback', 1
    )
    _mail(handle, rcpt, _report_message(rcpt, doctype, f"dtd-{stamp}"))
    # About a domain this nest does not serve → 250, dropped.
    foreign_id = f"foreign-{stamp}"
    _mail(
        handle,
        rcpt,
        _report_message(rcpt, _report_xml("not-ours.test", foreign_id), foreign_id),
    )

    # The 250 on `.` is the barrier: the hand-off to the nest completes inside
    # DATA, before the reply.
    with _admin_ws(handle.nest) as ws:
        listed = ws.call(
            "fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1}
        )
        reports = listed["reports"]
        ids = sorted(r["report_id"] for r in reports)
        assert ids == [good_id], (
            f"only the valid report about our own domain is stored; listed {ids}"
        )
        entry = reports[0]
        assert entry["our_domain"] == domain.lower()
        assert entry["submitter"] == "reporter.test <dmarc-noreply@reporter.test>"
        summary = entry["summary"]
        assert summary["message_count"] == 15, summary
        assert summary["disposition_reject"] == 3, summary
        assert summary["dmarc_pass"] == 12, summary
        assert summary["top_source_ips"][0] == {"value": "192.0.2.10", "count": 12}

        got = ws.call(
            "fauna.bridges.get_dmarc_aggregate_report",
            {
                "our_domain": entry["our_domain"],
                "submitter": entry["submitter"],
                "report_id": entry["report_id"],
            },
        )
        assert gzip.decompress(bytes(got["payload_xml_gz"])).decode() == _report_xml(
            domain, good_id
        ), "the archived XML is the report as received"

        health = ws.call("fauna.bridges.mail_health", {})
        assert health.get("last_inbound_accepted_at") is None, (
            "no report may reach a mailbox: this venue's inbound heartbeat moved, "
            f"so something was ingested into one ({health.get('last_inbound_accepted_at')})"
        )


def _forensic_message(rcpt: str, domain: str, tag: str) -> bytes:
    """An RFC 6591 auth-failure report: the forged message's headers as
    `text/rfc822-headers`, behind an RFC 5965 `multipart/report` wrapper."""
    lines = [
        "From: DMARC Reports <dmarc-noreply@reporter.test>",
        f"To: {rcpt}",
        "Subject: Authentication failure report",
        f"Message-ID: <{tag}@reporter.test>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        'Content-Type: multipart/report; report-type=feedback-report; boundary="rpt"',
        "",
        "--rpt",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "This is an authentication failure report.",
        "--rpt",
        "Content-Type: message/feedback-report",
        "",
        "Feedback-Type: auth-failure",
        "User-Agent: Reporter/1.0",
        "Version: 1",
        "Original-Mail-From: <bounce@forger.test>",
        "Original-Rcpt-To: <victim@peer.test>",
        "Arrival-Date: Sat, 23 May 2026 11:59:59 +0000",
        "Source-IP: 203.0.113.9",
        f"Reported-Domain: {domain}",
        "Auth-Failure: dmarc",
        "",
        "--rpt",
        "Content-Type: text/rfc822-headers",
        "",
        f"From: Billing <billing@{domain}>",
        "To: victim@peer.test",
        "Subject: Your invoice is overdue",
        f"Message-ID: <forged-{tag}@forger.test>",
        "",
        "--rpt--",
        "",
    ]
    return "\r\n".join(lines).encode()


@contextlib.contextmanager
def _forensic_opted_in(nest):
    """Opt the deployment in to forensic reports for the block, then withdraw
    the opt-in (a full PUT of the empty catalog) so later tests see the
    shipped default: the forensic leg stores nothing while ``ruf_publish`` is
    off (``dmarc-reporting.md`` § Forensic-report receive → *Default off*)."""
    with _admin_ws(nest) as ws:
        ws.call("fauna.bridges.put_dmarc_policy", {"ruf_publish": True})
    try:
        yield
    finally:
        with _admin_ws(nest) as ws:
            ws.call("fauna.bridges.put_dmarc_policy", {})


@pytest.mark.feature("dmarc-reporting")
def test_dmarc_forensic_report_is_stored_without_recipients_in_the_index(dmarc_venue):
    """A forensic (RFC 6591 auth-failure) report mailed to the same address is
    told apart from an aggregate one and stored in its own table: the admin's
    list names the failure but no recipient of the forged message, the full
    report is readable per report, an abuse-type feedback report is dropped,
    and none of them reaches a mailbox. The deployment opts in first: with
    ``ruf_publish`` off the nest stores no forensic report at all."""
    handle = dmarc_venue
    domain = handle.domain
    rcpt = f"dmarc-report@{domain}"
    tag = f"ruf-{int(time.time() * 1000)}"
    good = _forensic_message(rcpt, domain, tag)

    with _forensic_opted_in(handle.nest):
        _mail(handle, rcpt, good)
        # Strict wrapper: a feedback report that is not `auth-failure` → 250, dropped.
        _mail(
            handle,
            rcpt,
            _forensic_message(rcpt, domain, f"abuse-{tag}").replace(
                b"Feedback-Type: auth-failure", b"Feedback-Type: abuse"
            ),
        )

        with _admin_ws(handle.nest) as ws:
            listed = ws.call("fauna.bridges.list_dmarc_forensic_reports", {"window_days": 1})
            reports = [r for r in listed["reports"] if tag in r["reported_message_id"]]
            assert [r["reported_message_id"] for r in reports] == [
                f"forged-{tag}@forger.test"
            ], f"only the auth-failure report is stored; listed {listed['reports']}"
            entry = reports[0]
            assert entry["our_domain"] == domain.lower()
            assert entry["submitter"] == "DMARC Reports <dmarc-noreply@reporter.test>"
            summary = entry["summary"]
            assert summary["auth_failure"] == "dmarc", summary
            assert summary["source_ip"] == "203.0.113.9", summary
            assert summary["header_from"] == f"billing@{domain.lower()}", summary
            assert summary["partial"] is False, summary
            # The PII rule: the list read names no recipient and no subject.
            assert "victim" not in repr(listed), "a recipient reached the list read"
            assert "overdue" not in repr(listed), "a subject reached the list read"

            got = ws.call("fauna.bridges.get_dmarc_forensic_report", {"id": entry["id"]})
            archived = gzip.decompress(bytes(got["payload_raw_gz"]))
            # The MTA prepends its own `Received:` trace header; the rest is the
            # report mail byte for byte, recipients included.
            assert archived.startswith(b"Received: ") and archived.endswith(good), (
                f"the archived report is the report mail as received: {archived[:200]!r}"
            )

            # The forensic report did not land in the aggregate table.
            assert not any(
                tag in r["report_id"]
                for r in ws.call(
                    "fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1}
                )["reports"]
            )

            health = ws.call("fauna.bridges.mail_health", {})
            assert health.get("last_inbound_accepted_at") is None, (
                "no report may reach a mailbox: this venue's inbound heartbeat moved "
                f"({health.get('last_inbound_accepted_at')})"
            )


    # ── Outbound: the reports this nest sends peers ─────────────────────────────
    #
    # `dmarc-reporting.md` § Outbound aggregate-report sender: every inbound
    # message the MTA verifies is counted toward the domain its From claims, and
    # once a day the nest mails each peer whose `_dmarc` record publishes
    # `rua=mailto:` a gzipped RFC 7489 report through the ordinary outbound queue.
    # The pass runs from the test hook at a scripted moment against a stub TXT map
    # (the pass never touches DNS here); the report then leaves through the real
    # MTA to the venue's stub MX, which `external.test` routes to.


def _nest_hook(handle, path: str, body: dict | None = None) -> dict:
    import json
    import urllib.request

    req = urllib.request.Request(
        f"{handle.nest['url']}{path}",
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="GET" if body is None else "POST",
    )
    with urllib.request.urlopen(req, timeout=15.0) as resp:
        assert resp.status == 200, f"{path} returned {resp.status}"
        return json.loads(resp.read())


def _peer_message(sender: str, rcpt: str, tag: str) -> bytes:
    return "\r\n".join(
        [
            f"From: Peer newsletter <{sender}>",
            f"To: {rcpt}",
            f"Subject: {tag}",
            f"Message-ID: <{tag}@external.test>",
            "Date: Sat, 23 May 2026 12:00:00 +0000",
            "",
            "Counted toward the peer's DMARC report.",
            "",
        ]
    ).encode()


def _mail_as(handle, sender: str, rcpt: str, message: bytes) -> None:
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _report_attachment_xml(raw: bytes) -> str:
    import email
    from email import policy

    msg = email.message_from_bytes(raw, policy=policy.default)
    for part in msg.walk():
        if part.get_content_type() == "application/gzip":
            return gzip.decompress(part.get_payload(decode=True)).decode()
    raise AssertionError(f"no application/gzip report part in:\n{raw[:2000]!r}")


@pytest.mark.feature("mail-server")
def test_peer_publishing_a_mailto_rua_receives_our_aggregate_report(dmarc_venue):
    """Two messages from a peer domain reach the MTA; one daily pass later the
    peer's published `rua=mailto:` mailbox receives one gzipped RFC 7489 report
    from `dmarc-report@<our domain>` counting both, and the window is closed."""
    handle = dmarc_venue
    domain = handle.domain.lower()
    peer = "external.test"
    rcpt = f"dmarc-report@{domain}"
    stamp = int(time.time() * 1000)
    for i in range(2):
        sender = f"news@{peer}"
        _mail_as(handle, sender, rcpt, _peer_message(sender, rcpt, f"peer-{stamp}-{i}"))

    # The 250 on `.` is the barrier: the MTA reports the observation inside
    # DATA, before the reply.
    buckets = _nest_hook(handle, "/api/v1/test/outbound/dmarc_aggregator")["domains"]
    rows = buckets.get(peer, [])
    assert sum(r["count"] for r in rows) == 2, f"both messages counted for {peer}: {buckets!r}"
    assert {r["source_ip"] for r in rows} == {"127.0.0.1"}, rows

    queued = _nest_hook(
        handle,
        "/api/v1/test/outbound/emit_dmarc_now",
        {
            "now": int(time.time()),
            "txt": {f"_dmarc.{peer}": [f"v=DMARC1; p=none; rua=mailto:dmarc-agg@{peer}"]},
        },
    )["queued"]
    assert queued == 1, f"one report queued to the peer's mailbox, got {queued}"
    after = _nest_hook(handle, "/api/v1/test/outbound/dmarc_aggregator")["domains"]
    assert peer not in after, f"the pass closes the window: {after!r}"

    subject = f"Report Domain: {peer} Submitter: {domain}"
    raw = _wait_for_tagged(handle.stub_mx, subject, timeout=90.0)
    assert raw is not None, (
        f"the peer's MX never received the report ({subject!r}); "
        f"stub MX holds {len(handle.stub_mx.messages())} message(s)"
    )
    assert f"From: dmarc-report@{domain}".encode() in raw, raw[:600]
    xml = _report_attachment_xml(raw)
    for needle in (
        f"<org_name>{domain}</org_name>",
        f"<email>dmarc-report@{domain}</email>",
        f"<domain>{peer}</domain>",
        "<p>none</p>",
        "<source_ip>127.0.0.1</source_ip>",
        "<count>2</count>",
        f"<header_from>{peer}</header_from>",
    ):
        assert needle in xml, f"{needle} missing from the report:\n{xml}"

    sent = [
        r
        for r in _nest_hook(handle, "/api/v1/test/outbound/dmarc_sent")["rows"]
        if r["peer_domain"] == peer
    ]
    assert len(sent) == 1, sent
    assert sent[0]["destination"] == f"dmarc-agg@{peer}"
    assert sent[0]["xml"] == xml, "the archived report is the one sent"


def _reports_at_stub(handle, subject: str) -> list[bytes]:
    return [m for m in handle.stub_mx.messages() if subject.encode() in m]


@pytest.mark.feature("admin-mail-policy")
def test_outbound_dmarc_reports_switched_off_sends_no_report(dmarc_venue):
    """``dmarc-reporting.md`` § Generation is on by default: with
    ``mail.dmarc.outbound_aggregate_enabled`` off (``put_dmarc_policy``), the
    daily pass queues nothing for a peer that publishes ``rua=mailto:`` and
    discards the window it counted; switched back on, the next window's report
    reaches the peer's MX again, counting only the mail since."""
    handle = dmarc_venue
    domain = handle.domain.lower()
    peer = "external.test"
    rcpt = f"dmarc-report@{domain}"
    txt = {f"_dmarc.{peer}": [f"v=DMARC1; p=none; rua=mailto:dmarc-agg@{peer}"]}
    subject = f"Report Domain: {peer} Submitter: {domain}"
    stamp = int(time.time() * 1000)

    def sent_to_peer() -> int:
        rows = _nest_hook(handle, "/api/v1/test/outbound/dmarc_sent")["rows"]
        return sum(1 for r in rows if r["peer_domain"] == peer)

    def mail_from_peer(n: int, tag: str) -> None:
        for i in range(n):
            sender = f"news@{peer}"
            _mail_as(handle, sender, rcpt, _peer_message(sender, rcpt, f"{tag}-{stamp}-{i}"))

    with _admin_ws(handle.nest) as ws:
        try:
            ws.call("fauna.bridges.put_dmarc_policy", {"outbound_aggregate_enabled": False})
            got = ws.call("fauna.bridges.get_dmarc_policy", {})
            assert got["outbound_aggregate_enabled"] is False, got

            sent_before = sent_to_peer()
            at_stub_before = len(_reports_at_stub(handle, subject))
            mail_from_peer(2, "off")
            buckets = _nest_hook(handle, "/api/v1/test/outbound/dmarc_aggregator")["domains"]
            assert sum(r["count"] for r in buckets.get(peer, [])) == 2, buckets

            queued = _nest_hook(
                handle,
                "/api/v1/test/outbound/emit_dmarc_now",
                {"now": int(time.time()), "txt": txt},
            )["queued"]
            assert queued == 0, f"switched off, the pass queues nothing; queued {queued}"
            assert sent_to_peer() == sent_before, "no report recorded as sent while off"
            after = _nest_hook(handle, "/api/v1/test/outbound/dmarc_aggregator")["domains"]
            assert peer not in after, f"the window counted while off is discarded: {after!r}"

            # Back on (full PUT, the field absent ⇒ the default).
            ws.call("fauna.bridges.put_dmarc_policy", {})
            assert ws.call("fauna.bridges.get_dmarc_policy", {})["outbound_aggregate_enabled"] is True
            mail_from_peer(1, "on")
            queued = _nest_hook(
                handle,
                "/api/v1/test/outbound/emit_dmarc_now",
                {"now": int(time.time()), "txt": txt},
            )["queued"]
            assert queued == 1, f"switched back on, one report queued; queued {queued}"
        finally:
            ws.call("fauna.bridges.put_dmarc_policy", {})

    deadline = time.monotonic() + 90.0
    while time.monotonic() < deadline:
        reports = _reports_at_stub(handle, subject)
        if len(reports) > at_stub_before:
            break
        time.sleep(0.25)
    reports = _reports_at_stub(handle, subject)
    assert len(reports) == at_stub_before + 1, (
        f"exactly one new report reaches the peer's MX (none from the off window); "
        f"{at_stub_before} before, {len(reports)} now"
    )
    xml = _report_attachment_xml(reports[-1])
    assert "<count>1</count>" in xml, f"only the mail since switching on is counted:\n{xml}"
    assert sent_to_peer() == sent_before + 1


def _dmarc_record_tags(ws, domain: str) -> dict:
    """The tags of the ``_dmarc.<domain>`` record the nest expects to be
    published, read off ``fauna.dns.list_records`` (the one assembler both the
    DNS page and the provisioner consume)."""
    reply = ws.call("fauna.dns.list_records", {"domain": domain})
    row = next((d for d in reply.get("domains", []) if d.get("domain") == domain), None)
    assert row is not None, f"fauna.dns.list_records has no matrix for {domain!r}"
    dmarc = [
        r
        for r in row.get("records", [])
        if r["record_type"] == "TXT" and r["name"].rstrip(".") == f"_dmarc.{domain}"
    ]
    assert len(dmarc) == 1, f"expected one _dmarc record for {domain!r}: {dmarc!r}"
    return dict(
        part.strip().split("=", 1) for part in dmarc[0]["expected"].split(";") if "=" in part
    )


@pytest.mark.feature("dmarc-reporting")
def test_forensic_opt_in_asks_peers_for_reports_in_the_expected_record(dmarc_venue):
    """Forensic reports are off until an admin opts in (``dmarc-reporting.md``
    § Forensic-report receive → *Default off*): a fresh nest's expected ``_dmarc``
    record carries no ``ruf=``; the deployment-wide catalog write
    (``fauna.bridges.put_dmarc_policy``) with ``ruf_publish`` makes the record
    ask peers to send them to the nest's own report address with the chosen
    ``fo=`` mode, ``get_dmarc_policy`` reads the choice back, and a later write
    without the opt-in withdraws it (full PUT). The retention ceiling is
    enforced on the wire: a forensic retention past 90 days is refused and
    changes nothing (§ Forensic-report receive → *Retention*)."""
    handle = dmarc_venue
    domain = handle.domain
    report_address = f"mailto:dmarc-report@{domain}"

    with _admin_ws(handle.nest) as ws:
        before = _dmarc_record_tags(ws, domain)
        assert "ruf" not in before, f"a fresh nest must not publish ruf=: {before!r}"
        assert before.get("rua") == report_address
        policy = ws.call("fauna.bridges.get_dmarc_policy", {})
        assert policy["ruf_publish"] is False
        assert policy["forensic_retention_days"] == 30

        ws.call(
            "fauna.bridges.put_dmarc_policy",
            {"ruf_publish": True, "fo_mode": "d", "forensic_retention_days": 60},
        )
        after = _dmarc_record_tags(ws, domain)
        assert after.get("ruf") == report_address, f"ruf= missing after the opt-in: {after!r}"
        assert after.get("fo") == "d", after
        # The rest of the record is untouched by the opt-in.
        assert after.get("p") == before.get("p") == "reject"
        policy = ws.call("fauna.bridges.get_dmarc_policy", {})
        assert policy["ruf_publish"] is True
        assert policy["fo_mode"] == "d"
        assert policy["forensic_retention_days"] == 60

        # Past the ceiling: refused, and the stored choice stands.
        with pytest.raises(Exception) as refused:
            ws.call("fauna.bridges.put_dmarc_policy", {"forensic_retention_days": 91})
        assert "malformed" in str(refused.value), refused.value
        assert ws.call("fauna.bridges.get_dmarc_policy", {})["forensic_retention_days"] == 60

        # A write without the opt-in withdraws it — the catalog is a full PUT.
        ws.call("fauna.bridges.put_dmarc_policy", {})
        restored = _dmarc_record_tags(ws, domain)
        assert "ruf" not in restored, f"ruf= must go with the opt-in: {restored!r}"
        assert ws.call("fauna.bridges.get_dmarc_policy", {})["ruf_publish"] is False


def _oversize_message(rcpt: str, tag: str) -> bytes:
    """A report whose gzip inflates past the 500 MiB bound (a few hundred KiB on
    the wire, so it stays under the MTA's message cap)."""
    import zlib

    comp = zlib.compressobj(9, zlib.DEFLATED, 31)
    chunk = bytes(1024 * 1024)
    parts = [comp.compress(chunk) for _ in range(501)]
    parts.append(comp.flush())
    b64 = base64.encodebytes(b"".join(parts)).decode().replace("\n", "\r\n")
    lines = [
        "From: DMARC reporter <dmarc-noreply@reporter.test>",
        f"To: {rcpt}",
        f"Subject: Report domain: {tag}",
        f"Message-ID: <{tag}@reporter.test>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        'Content-Type: multipart/mixed; boundary="rpt"',
        "",
        "--rpt",
        f'Content-Type: application/gzip; name="{tag}.xml.gz"',
        "Content-Transfer-Encoding: base64",
        f'Content-Disposition: attachment; filename="{tag}.xml.gz"',
        "",
        b64.rstrip("\r\n"),
        "--rpt--",
        "",
    ]
    return "\r\n".join(lines).encode()


@pytest.mark.feature("dmarc-reporting")
def test_oversize_report_keeps_an_envelope_for_triage_and_retention_prunes_it(
    dmarc_venue,
):
    """An oversize report is accepted on the wire and dropped, but its envelope
    (no body) is kept and listed to the admin for ops triage; the daily
    retention prune, run at a scripted instant, keeps a report inside the 90-day
    window and removes it, with the triage row, past it
    (``dmarc-reporting.md`` § XML validation step 1 and § Retention)."""
    handle = dmarc_venue
    domain = handle.domain
    rcpt = f"dmarc-report@{domain}"
    stamp = int(time.time() * 1000)
    kept_id = f"retained-{stamp}"

    _mail(handle, rcpt, _report_message(rcpt, _report_xml(domain, kept_id), kept_id))
    _mail(handle, rcpt, _oversize_message(rcpt, f"huge-{stamp}"))

    with _admin_ws(handle.nest) as ws:
        listed = ws.call(
            "fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1}
        )
        ids = [r["report_id"] for r in listed["reports"]]
        assert kept_id in ids and len(ids) == len(set(ids)), ids
        assert not any(i.startswith("huge-") for i in ids), (
            "the oversize report must not be stored as a report"
        )
        assert len(listed["oversize"]) == 1, listed["oversize"]
        triage = listed["oversize"][0]
        assert triage["mail_from"] == "dmarc-noreply@reporter.test", triage
        assert triage["client_ip"], triage
        assert triage["size_bytes"] > 0, triage
        assert "payload" not in triage and "raw_message" not in triage, (
            "the triage row keeps the envelope minus the body"
        )
        received_at = max(r["received_at"] for r in listed["reports"])
        day = 86_400

        # Inside the window: the report and the triage row stay.
        _nest_hook(
            handle, "/api/v1/test/dmarc/prune_now", {"now": received_at + 89 * day}
        )
        inside = ws.call(
            "fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1}
        )
        assert kept_id in [r["report_id"] for r in inside["reports"]]
        assert len(inside["oversize"]) == 1

        # Past the 90-day retention: both are pruned.
        _nest_hook(
            handle, "/api/v1/test/dmarc/prune_now", {"now": received_at + 91 * day}
        )
        past = ws.call(
            "fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1}
        )
        assert past["reports"] == [], past["reports"]
        assert past["oversize"] == [], past["oversize"]


@pytest.mark.feature("dmarc-reporting")
def test_tuned_retention_drives_the_prune_and_the_oversize_envelope_goes_with_it(
    dmarc_venue,
):
    """The prune follows the retention the admin saved, not the 90-day default
    (``dmarc-reporting.md`` § Retention: ``mail.dmarc.aggregate_retention_days``,
    read by the daily sweep through the effective policy): with the retention
    tuned to 10 days, a report and an oversize report's triage envelope stay at
    day 9 and are both removed at day 11 — long before the default would."""
    handle = dmarc_venue
    domain = handle.domain
    rcpt = f"dmarc-report@{domain}"
    stamp = int(time.time() * 1000)
    kept_id = f"tuned-{stamp}"
    day = 86_400

    with _admin_ws(handle.nest) as ws:
        try:
            ws.call("fauna.bridges.put_dmarc_policy", {"aggregate_retention_days": 10})
            assert ws.call("fauna.bridges.get_dmarc_policy", {})[
                "aggregate_retention_days"
            ] == 10

            _mail(handle, rcpt, _report_message(rcpt, _report_xml(domain, kept_id), kept_id))
            _mail(handle, rcpt, _oversize_message(rcpt, f"huge-tuned-{stamp}"))
            listed = ws.call("fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1})
            mine = [r for r in listed["reports"] if r["report_id"] == kept_id]
            assert len(mine) == 1, listed["reports"]
            assert listed["oversize"], "the oversize report left its triage envelope"
            received_at = mine[0]["received_at"]

            _nest_hook(handle, "/api/v1/test/dmarc/prune_now", {"now": received_at + 9 * day})
            inside = ws.call("fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1})
            assert kept_id in [r["report_id"] for r in inside["reports"]], (
                "inside the tuned 10-day window the report stays"
            )
            assert inside["oversize"], "inside the window the triage envelope stays"

            _nest_hook(handle, "/api/v1/test/dmarc/prune_now", {"now": received_at + 11 * day})
            past = ws.call("fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1})
            assert kept_id not in [r["report_id"] for r in past["reports"]], (
                "past the tuned retention the report is pruned, though the "
                f"90-day default would keep it: {past['reports']}"
            )
            assert past["oversize"] == [], (
                f"the triage envelope goes with the reports: {past['oversize']}"
            )
        finally:
            ws.call("fauna.bridges.put_dmarc_policy", {})


@pytest.mark.feature("dmarc-reporting")
def test_deployment_dmarc_setting_is_every_domains_base_and_a_per_domain_reject_stands(
    dmarc_venue,
):
    """``dmarc-reporting.md`` § DMARC policy publication → *Multi-domain
    deployments*: the deployment-wide catalog (``put_dmarc_policy``) is the base
    every domain's expected ``_dmarc`` record is built on, and a domain's own
    ``reject`` (``update_local_domain_config`` → ``dmarc_policy_mode``) is kept
    and published even while the deployment is softened to ``p=none``."""
    import secrets

    handle = dmarc_venue
    primary = handle.domain
    second = f"d{secrets.token_hex(4)}.test"

    with _admin_ws(handle.nest) as ws:
        try:
            ws.call("fauna.bridges.put_dmarc_policy", {"policy_mode": "none", "pct": 50})
            ws.call(
                "fauna.bridges.add_local_domain",
                {"domain": second, "mta_sts_cert_mode": "per_host"},
            )

            # The softened deployment is the base of both domains' records.
            for domain in (primary, second):
                tags = _dmarc_record_tags(ws, domain)
                assert tags.get("p") == "none", f"{domain}: {tags!r}"
                assert tags.get("pct") == "50", f"{domain}: {tags!r}"

            # The second domain is made stricter than the deployment.
            ws.call(
                "fauna.bridges.update_local_domain_config",
                {"domain": second, "dmarc_policy_mode": "reject"},
            )
            strict = _dmarc_record_tags(ws, second)
            assert strict.get("p") == "reject", (
                f"a per-domain reject must stand under a p=none deployment: {strict!r}"
            )
            assert strict.get("sp") == "reject", strict
            assert strict.get("pct") == "50", (
                f"the rest of the deployment base still applies: {strict!r}"
            )
            soft = _dmarc_record_tags(ws, primary)
            assert soft.get("p") == "none", (
                f"another domain's override leaves the primary on the base: {soft!r}"
            )
        finally:
            with contextlib.suppress(Exception):
                ws.call("fauna.bridges.remove_local_domain", {"domain": second})
            ws.call("fauna.bridges.put_dmarc_policy", {})


@pytest.mark.feature("dmarc-reporting")
def test_dmarc_policy_save_asking_the_impossible_is_refused(dmarc_venue):
    """``dmarc-reporting.md`` § Implementation status today (the
    ``put_dmarc_policy`` refusals): a sample rate over 100, a report destination
    that is not exactly one plain RFC 7489 ``mailto:`` dmarc-uri, one longer than
    the 72 bytes that keep the record one TXT string, and a retention outside
    its bounds are each refused as malformed, and the stored policy stands."""
    handle = dmarc_venue
    domain = handle.domain
    # Within RFC 5321's 64-byte local part, but past 72 bytes as a whole.
    long_local = "r" * 60
    impossible = [
        {"pct": 101},
        {"rua_destination": f"mailto:a@{domain},mailto:b@{domain}"},
        {"rua_destination": "https://reports.example.test/dmarc"},
        {"rua_destination": f'mailto:"a b"@{domain}'},
        {"rua_destination": f"mailto:a@{domain}\r\nBcc: x@{domain}"},
        {"rua_destination": f"mailto:{long_local}@{domain}"},
        {"aggregate_retention_days": 0},
        {"aggregate_retention_days": 366},
        {"forensic_retention_days": 91},
    ]

    with _admin_ws(handle.nest) as ws:
        try:
            ws.call("fauna.bridges.put_dmarc_policy", {"pct": 40})
            before = ws.call("fauna.bridges.get_dmarc_policy", {})
            assert before["pct"] == 40, before
            for body in impossible:
                with pytest.raises(Exception) as refused:
                    ws.call("fauna.bridges.put_dmarc_policy", body)
                assert "malformed" in str(refused.value), (body, refused.value)
                after = ws.call("fauna.bridges.get_dmarc_policy", {})
                assert after == before, (
                    f"a refused save changed the stored policy ({body!r}): "
                    f"{before!r} -> {after!r}"
                )
            assert _dmarc_record_tags(ws, domain).get("pct") == "40"
        finally:
            ws.call("fauna.bridges.put_dmarc_policy", {})


@pytest.mark.feature("dmarc-reporting")
def test_no_forensic_report_is_kept_while_forensic_reports_are_not_opted_in(
    dmarc_venue,
):
    """``dmarc-reporting.md`` § Forensic-report receive (RUF) → *Default off*:
    while ``ruf_publish`` is off the nest keeps no forensic report whatever a
    sender mails — the mail is accepted on the wire (250) and nothing is stored,
    and no copy reaches a mailbox."""
    handle = dmarc_venue
    domain = handle.domain
    rcpt = f"dmarc-report@{domain}"
    tag = f"ruf-off-{int(time.time() * 1000)}"

    with _admin_ws(handle.nest) as ws:
        assert ws.call("fauna.bridges.get_dmarc_policy", {})["ruf_publish"] is False
        _mail(handle, rcpt, _forensic_message(rcpt, domain, tag))
        listed = ws.call("fauna.bridges.list_dmarc_forensic_reports", {"window_days": 1})
        assert not any(tag in r["reported_message_id"] for r in listed["reports"]), (
            f"a forensic report was stored while not opted in: {listed['reports']}"
        )
        assert not any(
            tag in r["report_id"]
            for r in ws.call("fauna.bridges.list_dmarc_aggregate_reports", {"window_days": 1})[
                "reports"
            ]
        ), "the forensic report must not land in the aggregate table either"
        health = ws.call("fauna.bridges.mail_health", {})
        assert health.get("last_inbound_accepted_at") is None, (
            "no report may reach a mailbox: this venue's inbound heartbeat moved "
            f"({health.get('last_inbound_accepted_at')})"
        )


def _mail_as_any_verdict(handle, sender: str, rcpt: str, message: bytes) -> str:
    """Mail ``message`` and return the server's final reply to ``.``, whatever
    its code — the MTA counts the observation before any enforcement gate, so
    whether it then delivers or refuses is not this caller's question."""
    from helpers.mail_wire import _read_final_reply

    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(message)
        conn.send_raw(b".\r\n")
        reply = _read_final_reply(conn, deadline)
        with contextlib.suppress(Exception):
            conn.cmd("QUIT", "221", deadline)
    return reply


@pytest.mark.feature("dmarc-reporting")
def test_daily_report_goes_only_where_allowed_and_never_counts_our_own_domains(
    dmarc_venue,
):
    """``dmarc-reporting.md`` § Outbound aggregate-report sender: mail whose From
    claims one of our own domains, or a name beneath one, is not counted; the
    daily pass skips an ``https:`` destination, a third-party mailbox that has
    not authorised the report (no ``<peer>._report._dmarc.<its domain>`` record),
    and a destination carrying bytes that could forge a header — so the one
    report goes to the peer's own mailbox alone, and nothing a stranger wrote
    reaches its ``To:``."""
    handle = dmarc_venue
    domain = handle.domain.lower()
    peer = "external.test"
    rcpt = f"dmarc-report@{domain}"
    stamp = int(time.time() * 1000)

    # Mail claiming our own domain, and a name beneath it, is never counted.
    for claimed in (domain, f"news.{domain}"):
        sender = f"billing@{claimed}"
        _mail_as_any_verdict(
            handle, f"bounce@{peer}", rcpt, _peer_message(sender, rcpt, f"own-{stamp}")
        )
    sender = f"news@{peer}"
    sent_before = {
        r["report_id"] for r in _nest_hook(handle, "/api/v1/test/outbound/dmarc_sent")["rows"]
    }
    subject = f"Report Domain: {peer} Submitter: {domain}"
    at_stub_before = len(_reports_at_stub(handle, subject))

    def one_pass(n: int, rua: list[str]) -> int:
        """Count one peer message, then run a daily pass whose peer record
        publishes ``rua`` (two URIs: the sender honours the first two,
        § step 2). The message makes the window non-empty, so a pass that
        queues nothing has refused its destinations, not found no mail."""
        _mail_as(handle, sender, rcpt, _peer_message(sender, rcpt, f"allowed-{stamp}-{n}"))
        buckets = _nest_hook(handle, "/api/v1/test/outbound/dmarc_aggregator")["domains"]
        assert not any(d == domain or d.endswith(f".{domain}") for d in buckets), (
            f"mail claiming our own domains was counted: {sorted(buckets)}"
        )
        rows = buckets.get(peer, [])
        assert sum(r["count"] for r in rows) == 1, buckets
        assert {r["disposition"] for r in rows} == {"none"}, (
            f"delivered mail is reported with disposition none: {rows!r}"
        )
        record = f"v=DMARC1; p=none; rua={', '.join(rua)}"
        return _nest_hook(
            handle,
            "/api/v1/test/outbound/emit_dmarc_now",
            {"now": int(time.time()), "txt": {f"_dmarc.{peer}": [record]}},
        )["queued"]

    # A destination smuggling a header, and a third party that never
    # authorised the report: nothing goes.
    forged = f"mailto:dmarc-agg@{peer}\r\nBcc: evil@attacker.test"
    queued = one_pass(1, [forged, "mailto:agg@unauthorised.test"])
    assert queued == 0, f"a forged or unauthorised destination got a report; queued {queued}"
    # A quoted local part and an https: URI: nothing goes.
    queued = one_pass(2, [f'mailto:"x"@{peer}', f"https://{peer}/dmarc"])
    assert queued == 0, f"a quoted or https: destination got a report; queued {queued}"
    # The peer's own plain mailbox, behind an https: URI: one report, there alone.
    queued = one_pass(3, [f"https://{peer}/dmarc", f"mailto:dmarc-agg@{peer}"])
    assert queued == 1, f"one report, to the peer's own mailbox alone; queued {queued}"

    new = [
        r
        for r in _nest_hook(handle, "/api/v1/test/outbound/dmarc_sent")["rows"]
        if r["report_id"] not in sent_before
    ]
    assert [r["destination"] for r in new] == [f"dmarc-agg@{peer}"], new

    deadline = time.monotonic() + 90.0
    while time.monotonic() < deadline and len(_reports_at_stub(handle, subject)) <= at_stub_before:
        time.sleep(0.25)
    reports = _reports_at_stub(handle, subject)
    assert len(reports) == at_stub_before + 1, (
        f"exactly one new report reaches the peer's MX; {at_stub_before} before, "
        f"{len(reports)} now"
    )
    head = reports[-1].split(b"\r\n\r\n", 1)[0]
    assert b"attacker.test" not in head and b"unauthorised.test" not in head, (
        f"a stranger's bytes reached the report's headers:\n{head.decode(errors='replace')}"
    )
    to = [line for line in head.split(b"\r\n") if line.lower().startswith(b"to:")]
    assert to == [f"To: dmarc-agg@{peer}".encode()], to


# ── A message the perimeter refuses is counted with disposition reject ──


@pytest.fixture(scope="module")
def reject_peer_mta(dmarc_venue, mail_bridge_e2e_binary, seal_helper_binary, tmp_path_factory):
    """An MTA from the e2e flavor of the bridge beside the venue's nest, whose
    sender-authentication lookups go to a loopback ``FakeDns`` this fixture runs
    (fauna-mail's ``FAUNA_MAIL_E2E_RESOLVER`` seam, compiled out of the shipped
    bridge — convention 15). The fake publishes, for a peer domain of this
    fixture's own, ``_dmarc`` ``p=reject`` and an SPF record that admits no
    sender, so the peer's unsigned mail from loopback fails DMARC under a reject
    policy. Yields ``(spawned bridge, peer domain)``."""
    from conftest import _spawn_mta_bridge
    from fakes.fake_dns import FakeDns

    handle = dmarc_venue
    peer = f"reject-{uuid.uuid4().hex[:10]}.test"
    dns = FakeDns(
        host="127.0.0.1",
        port=0,
        txt_records={
            f"_dmarc.{peer}": "v=DMARC1; p=reject",
            peer: "v=spf1 -all",
        },
    ).start()
    try:
        spawned = _spawn_mta_bridge(
            mail_bridge_binary=mail_bridge_e2e_binary,
            seal_helper_binary=seal_helper_binary,
            nest_instance=handle.nest,
            tmp=tmp_path_factory.mktemp("mail-bridge-mta-reject-peer"),
            bridge_id=f"test-mta-reject-peer-{uuid.uuid4().hex[:12]}",
            domain=handle.domain,
            extra_env={"FAUNA_MAIL_E2E_RESOLVER": f"127.0.0.1:{dns.port}"},
        )
        try:
            yield spawned, peer
        finally:
            spawned.cleanup()
    finally:
        dns.stop()


@pytest.mark.feature("dmarc-reporting")
def test_peer_mail_the_perimeter_refuses_is_counted_with_disposition_reject(
    dmarc_venue, reject_peer_mta,
):
    """``dmarc-reporting.md`` § Outbound aggregate-report sender, step 1: a
    message the perimeter refuses is still counted, with disposition
    ``reject``. A peer publishing ``p=reject`` mails us unsigned from an address
    its SPF does not admit; with the DMARC enforcement switch on, the MTA
    answers ``550 5.7.1`` to the message — and, having reported its observation
    before the enforcement gate, the nest's aggregate for the peer holds the one
    message with disposition ``reject``."""
    from helpers.mail_wire import _read_final_reply

    handle = dmarc_venue
    spawned, peer = reject_peer_mta
    domain = handle.domain.lower()
    rcpt = f"dmarc-report@{domain}"
    sender = f"news@{peer}"

    before = _nest_hook(handle, "/api/v1/test/outbound/dmarc_aggregator")["domains"]
    assert peer not in before, f"{peer} counted before it mailed: {before!r}"
    # The switch's own default (`AuthPolicy::default`), set explicitly so the
    # witness states what it needs; the two fields are partial overrides, so
    # every other auth setting of the shared venue stands. Being the default,
    # the reject-peer MTA's boot snapshot already holds them, so no wait on the
    # hot reload is needed.
    with _admin_ws(handle.nest) as ws:
        ws.call("fauna.bridges.put_auth_policy", {"enforce_dmarc": True, "log_only": False})

    tag = f"reject-{int(time.time() * 1000)}"
    deadline = time.monotonic() + 60.0
    with _connect_smtp_starttls(spawned.mx_port, domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(_peer_message(sender, rcpt, tag))
        conn.send_raw(b".\r\n")
        reply = _read_final_reply(conn, deadline)
        with contextlib.suppress(Exception):
            conn.cmd("QUIT", "221", deadline)
    assert reply.startswith("550") and "5.7.1" in reply, (
        f"peer mail failing its own p=reject DMARC must be refused 550 5.7.1 at DATA; "
        f"got {reply!r} (bridge log {spawned.log_file_path})"
    )

    # The reply to `.` is the barrier: the MTA reports the observation inside
    # DATA, before its enforcement gate answers.
    rows = _nest_hook(handle, "/api/v1/test/outbound/dmarc_aggregator")["domains"].get(peer, [])
    assert sum(r["count"] for r in rows) == 1, f"the refused message is counted once: {rows!r}"
    assert {r["disposition"] for r in rows} == {"reject"}, (
        f"a message the perimeter refused is counted with disposition reject: {rows!r}"
    )
    assert {r["source_ip"] for r in rows} == {"127.0.0.1"}, rows
