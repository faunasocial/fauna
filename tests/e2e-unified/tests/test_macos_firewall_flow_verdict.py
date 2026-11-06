"""tier_1: reading the application firewall's verdict on a nest's inbound flow.

`helpers.macos_firewall.last_flow_verdict` parses `socketfilterfw`'s own unified
log, because `--listapps` cannot answer the question: an Allow entry for a
binary's path says nothing when an entry for its signing identifier decides the
flow first. The log lines
below are the shapes measured on macOS on 2026-10-30 and 2026-10-31.
"""
from __future__ import annotations

import pytest

from helpers.macos_firewall import last_flow_verdict

pytestmark = [pytest.mark.tier_1]

PREFIX = "2026-10-31 18:24:13.236 Df socketfilterfw[267:f2664f] [com.apple.ALF.ApplicationFirewall:fw] "
BLOCKED = (
    PREFIX + "KNOWN APP FLOW: file:///w/mac-3/target/debug/fauna-nest-e2e-dba785f8, "
    "return known verdict: 2 (BlockAll set: 0)"
)
ALLOWED = (
    PREFIX + "KNOWN APP FLOW: file:///w/mac-3/target/debug/fauna-nest-e2e-dba785f8, "
    "return known verdict: 1"
)
HELD = PREFIX + "Enqueuing new inbound flow for fauna-nest-e2e-dba785f8 with identifier: fauna_nest"
OTHER_APP = PREFIX + "KNOWN APP FLOW: file:///usr/sbin/mDNSResponder, return known verdict: 1"


@pytest.mark.parametrize(
    "lines, want",
    [
        ([BLOCKED], "blocked"),
        ([ALLOWED], "allowed"),
        ([HELD], "held"),
        ([BLOCKED, OTHER_APP], "blocked"),
        ([HELD, BLOCKED], "blocked"),
        ([BLOCKED, HELD], "held"),
        ([OTHER_APP], None),
        ([], None),
    ],
)
def test_the_newest_flow_for_the_binary_decides(lines, want):
    assert last_flow_verdict("\n".join(lines), "fauna-nest") == want


def test_an_unknown_verdict_number_is_not_guessed():
    line = BLOCKED.replace("verdict: 2", "verdict: 7")
    assert last_flow_verdict(line, "fauna-nest") is None
