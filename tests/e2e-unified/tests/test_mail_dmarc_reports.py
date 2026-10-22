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
import gzip
import time

import pytest

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


@pytest.mark.feature("dmarc-reporting")
def test_dmarc_forensic_report_is_stored_without_recipients_in_the_index(dmarc_venue):
    """A forensic (RFC 6591 auth-failure) report mailed to the same address is
    told apart from an aggregate one and stored in its own table: the admin's
    list names the failure but no recipient of the forged message, the full
    report is readable per report, an abuse-type feedback report is dropped,
    and none of them reaches a mailbox."""
    handle = dmarc_venue
    domain = handle.domain
    rcpt = f"dmarc-report@{domain}"
    tag = f"ruf-{int(time.time() * 1000)}"
    good = _forensic_message(rcpt, domain, tag)

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
