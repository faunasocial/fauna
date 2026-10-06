"""tier_1 unit tests for the stub-MX auto-reply builder
(`helpers.mail_wire.build_reply_message` + `header_value` / `parse_addr`).

Pure-Python, no nest / no driver: these pin the header parsing + reply synthesis
the tier_3 auto-reply round-trip (`test_mail_client_reply_roundtrip.py`) depends
on, so a regression in the parser is caught in <1s instead of inside a slow e2e.
"""

import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from helpers.mail_wire import build_reply_message, header_value, parse_addr


def _msg(*lines: str) -> bytes:
    return ("\r\n".join(lines) + "\r\n").encode()


class TestHeaderValue:
    def test_reads_simple_header(self):
        raw = _msg("From: a@b.test", "Subject: hi", "", "body")
        assert header_value(raw, "Subject") == "hi"

    def test_case_insensitive_field_name(self):
        raw = _msg("From: a@b.test", "", "body")
        assert header_value(raw, "from") == "a@b.test"

    def test_absent_header_is_none(self):
        raw = _msg("From: a@b.test", "", "body")
        assert header_value(raw, "Cc") is None

    def test_unfolds_continuation_lines(self):
        raw = _msg("Subject: a very", " long folded", "\tsubject", "", "body")
        assert header_value(raw, "Subject") == "a very long folded subject"

    def test_bare_lf_separator(self):
        raw = b"From: a@b.test\nSubject: hi\n\nbody\n"
        assert header_value(raw, "Subject") == "hi"

    def test_first_occurrence_wins(self):
        raw = _msg("Subject: first", "Subject: second", "", "body")
        assert header_value(raw, "Subject") == "first"


class TestParseAddr:
    def test_display_name_form(self):
        assert parse_addr("Alice Example <alice@fauna.test>") == "alice@fauna.test"

    def test_bare_address(self):
        assert parse_addr("alice@fauna.test") == "alice@fauna.test"

    def test_none_is_empty(self):
        assert parse_addr(None) == ""

    def test_strips_whitespace(self):
        assert parse_addr("  <a@b.test>  ") == "a@b.test"


class TestBuildReplyMessage:
    def _outbound(self):
        return _msg(
            "From: Alice <alice@fauna.test>",
            "To: bob@external.test",
            "Subject: Hello there",
            "Message-ID: <orig-123@fauna.test>",
            "Date: Mon, 01 Jun 2026 10:00:00 +0000",
            "",
            "Outbound body.",
        )

    def test_reply_to_is_parsed_from_original_from(self):
        reply_to, _ = build_reply_message(
            self._outbound(), reply_nonce="NONCE9", reply_from="bob@external.test"
        )
        assert reply_to == "alice@fauna.test"

    def test_reply_addressed_to_original_sender(self):
        _, raw = build_reply_message(
            self._outbound(), reply_nonce="NONCE9", reply_from="bob@external.test"
        )
        assert header_value(raw, "To") == "alice@fauna.test"
        assert parse_addr(header_value(raw, "From")) == "bob@external.test"

    def test_subject_is_re_prefixed_and_carries_nonce(self):
        _, raw = build_reply_message(
            self._outbound(), reply_nonce="NONCE9", reply_from="bob@external.test"
        )
        subject = header_value(raw, "Subject")
        assert subject.startswith("Re: ")
        assert "Hello there" in subject
        assert "NONCE9" in subject

    def test_does_not_double_prefix_existing_re(self):
        outbound = _msg(
            "From: Alice <alice@fauna.test>",
            "Subject: Re: already a reply",
            "",
            "body",
        )
        _, raw = build_reply_message(
            outbound, reply_nonce="N", reply_from="bob@external.test"
        )
        # Exactly one "Re: " prefix, not "Re: Re: ".
        assert header_value(raw, "Subject").startswith("Re: already a reply")
        assert "Re: Re:" not in header_value(raw, "Subject")

    def test_nonce_in_body(self):
        _, raw = build_reply_message(
            self._outbound(), reply_nonce="NONCE9", reply_from="bob@external.test"
        )
        assert b"NONCE9" in raw.split(b"\r\n\r\n", 1)[1]

    def test_threads_original_message_id(self):
        _, raw = build_reply_message(
            self._outbound(), reply_nonce="N", reply_from="bob@external.test"
        )
        assert header_value(raw, "In-Reply-To") == "<orig-123@fauna.test>"
        assert header_value(raw, "References") == "<orig-123@fauna.test>"

    def test_no_message_id_omits_threading_headers(self):
        outbound = _msg("From: Alice <alice@fauna.test>", "Subject: x", "", "body")
        _, raw = build_reply_message(
            outbound, reply_nonce="N", reply_from="bob@external.test"
        )
        assert header_value(raw, "In-Reply-To") is None

    def test_wire_is_crlf_terminated_bytes(self):
        _, raw = build_reply_message(
            self._outbound(), reply_nonce="N", reply_from="bob@external.test"
        )
        assert isinstance(raw, bytes)
        assert raw.endswith(b"\r\n")
        assert b"\r\n\r\n" in raw  # header/body separator
