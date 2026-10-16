"""What the nest does with a helper's reported events before an admin sees them
(`docs/goal/architecture/apps/observability.md` § Admission, attribution, and
the remote ring (nest-side)).

`docs/features/admin-logs.md` outcomes 5, 6 and 7, at the wire. An enrolled,
approved helper — an RPC client here, its key enrolled the way the helper's own
cold boot does — reports through `fauna.bridges.report_log_events`, and the
admin reads `fauna.admin.logs`. Admission is synchronous in the reporting call,
so every read follows its report without a wait (convention 14). Each test
starts its own nest: the per-helper rate-limit buckets are process state, so a
shared nest would hand one test another's spent tokens.
"""

from __future__ import annotations

import secrets
import time
import unicodedata

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.bridge_enrollment import enroll_and_approve_bridge

pytestmark = pytest.mark.tier_3

# The protocol's batch cap (`fauna_protocol::log_plane::MAX_EVENTS_PER_BATCH`)
# and message cap (`MAX_MESSAGE_BYTES`): a batch this size is one the nest
# admits in full shape, so every drop below is the rate limit's.
BATCH = 128
MESSAGE_CAP_BYTES = 512

HELPER_PREFIXES = ("mta:", "mda:", "atproto:", "content_processor:", "relay:")


@pytest.fixture
def log_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "log-admission-nest")
    raw = nest["admin"]["signing_key"]
    nest["admin_sk"] = raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))
    yield nest
    cleanup()


def _helper(nest, role: str) -> SigningKey:
    key = SigningKey.generate()
    enroll_and_approve_bridge(nest["url"], nest["admin_sk"], bytes(key.verify_key), role)
    return key


def _report(nest, helper: SigningKey, events: list[dict]) -> None:
    with WsRpcAdminClient(nest["url"], actor_id=bytes(helper.verify_key),
                          signing_key=bytes(helper)) as ws:
        ws.call("fauna.bridges.report_log_events", {"events": events, "dropped": 0})


def _event(event: str, message: str, level: str = "warn") -> dict:
    return {"timestamp_ms": int(time.time() * 1000), "level": level,
            "event": event, "message": message}


def _admin_log(nest) -> list[dict]:
    sk = nest["admin_sk"]
    with WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key),
                          signing_key=bytes(sk)) as admin:
        return admin.call("fauna.admin.logs", {})["entries"]


def _is_nest_own(entry: dict) -> bool:
    return not entry["target"].startswith(HELPER_PREFIXES)


@pytest.mark.feature("admin-logs")
def test_a_flooding_helper_never_pushes_the_nests_own_history_out(log_nest):
    """Outcome 5: every line the nest had written before two helpers flood the
    log is still there after, and the oldest is still the oldest."""
    nest = log_nest
    before = [e for e in _admin_log(nest) if _is_nest_own(e)]
    assert before, "a fresh nest's log carries its own start-up history"

    tag = secrets.token_hex(3)
    for role in ("mta", "mda"):
        helper = _helper(nest, role)
        for n in range(3):
            _report(nest, helper, [_event(f"flood.{tag}.{n}", f"noise {i}") for i in range(BATCH)])

    after = _admin_log(nest)
    flooded = [e for e in after if f"flood.{tag}." in e["target"]]
    assert flooded, f"the flood never reached the log: {after[-5:]!r}"

    def key(e):
        return (e["timestamp_ms"], e["level"], e["target"], e["message"])

    after_own = [e for e in after if _is_nest_own(e)]
    missing = [e for e in before if key(e) not in {key(a) for a in after_own}]
    assert not missing, f"the helpers' flood evicted {len(missing)} of the nest's own lines: {missing[:3]!r}"
    assert key(after_own[0]) == key(before[0]), (
        "the nest's oldest line is no longer the oldest it keeps after the flood"
    )


@pytest.mark.feature("admin-logs")
def test_a_flood_is_dropped_and_announced_once_a_minute_naming_the_count(log_nest):
    """Outcome 6: a helper over its allowance loses the excess, and one nest
    warning names how many of its events were lost; a second flood within the
    minute adds no second warning."""
    nest = log_nest
    helper = _helper(nest, "mta")
    tag = secrets.token_hex(3)

    _report(nest, helper, [_event(f"first.{tag}", f"n{i}") for i in range(BATCH)])
    log = _admin_log(nest)
    admitted = [e for e in log if e["target"] == f"mta:first.{tag}"]
    assert 0 < len(admitted) < BATCH, (
        f"a {BATCH}-event burst must be cut by the rate limit, admitted {len(admitted)}"
    )
    warnings = [e for e in log if _is_nest_own(e) and "from mta" in e["message"]]
    assert len(warnings) == 1, f"one warning must announce the drop: {warnings!r}"
    assert warnings[0]["level"] == "Warn", warnings[0]
    lost = BATCH - len(admitted)
    assert f"dropped {lost} event" in warnings[0]["message"], (
        f"the warning must name the {lost} events lost: {warnings[0]!r}"
    )

    _report(nest, helper, [_event(f"second.{tag}", f"n{i}") for i in range(BATCH)])
    warnings = [e for e in _admin_log(nest) if _is_nest_own(e) and "from mta" in e["message"]]
    assert len(warnings) == 1, (
        f"a second flood inside the minute must not add a second warning: {warnings!r}"
    )


@pytest.mark.feature("admin-logs")
def test_a_helpers_line_arrives_stripped_bounded_and_at_an_admitted_level(log_nest):
    """Outcome 7: control characters are stripped, the message is cut to a
    bounded length, and only error, warning and information lines are kept."""
    nest = log_nest
    helper = _helper(nest, "mda")
    tag = secrets.token_hex(3)
    _report(nest, helper, [
        _event(f"ctl.{tag}", "red\x1b[31m text\x07\nforged line\r\tend", "info"),
        _event(f"long.{tag}", "x" * 4 * MESSAGE_CAP_BYTES, "warn"),
        _event(f"err.{tag}", "kept", "error"),
        _event(f"dbg.{tag}", "never kept", "debug"),
        _event(f"odd.{tag}", "never kept", "critical"),
    ])
    mine = {e["target"]: e for e in _admin_log(nest) if tag in e["target"]}

    ctl = mine.get(f"mda:ctl.{tag}")
    assert ctl, f"the info line never arrived: {sorted(mine)!r}"
    assert not [c for c in ctl["message"] if unicodedata.category(c) == "Cc"], (
        f"control characters survived into the log: {ctl['message']!r}"
    )
    assert "forged line" in ctl["message"] and "end" in ctl["message"], ctl

    long = mine.get(f"mda:long.{tag}")
    assert long, f"the long warning never arrived: {sorted(mine)!r}"
    assert 0 < len(long["message"].encode()) <= MESSAGE_CAP_BYTES, (
        f"the message must be cut to {MESSAGE_CAP_BYTES} bytes, got {len(long['message'].encode())}"
    )

    assert mine.get(f"mda:err.{tag}", {}).get("level") == "Error", mine
    assert {e["level"] for e in mine.values()} <= {"Error", "Warn", "Info"}, mine
    assert f"mda:dbg.{tag}" not in mine and f"mda:odd.{tag}" not in mine, (
        f"a debug or unknown-level line was kept: {sorted(mine)!r}"
    )
