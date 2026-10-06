"""Shared inbox-decode/poll helpers for the MDA server-side auto-schedule
**sealed-rail** tier_3 tests — the same-nest `test_caldav_autoschedule_mailbox_less.py`
and the cross-nest twin `test_caldav_autoschedule_cross_nest.py`. One copy of the
`WelcomeKind::Scheduling` inbox decode + poll, not a per-test copy (priority #2/#4).

The decode mirrors `libs/fauna-protocol/src/inbox.rs`: an `InboxEnvelope` is a map
`{"kind": "welcome", "payload": <bstr>}` whose payload decodes to `{"welcome_bytes":
<bstr>, "channel_id": hex?, "channel_type": "scheduling"?, ...}`. A non-Welcome
envelope (security notice / contact request) or a non-scheduling welcome is skipped,
so the count is exactly the scheduling deliveries.
"""

from __future__ import annotations

import time

import cbor2

from tests.api import conv_api


def utc_offset(offset_min: int) -> str:
    """ISO basic-format UTC `offset_min` minutes from now (whole minutes)."""
    t = time.gmtime(time.time() + offset_min * 60)
    return time.strftime("%Y%m%dT%H%M00Z", t)


def scheduling_welcomes(inbox_items: list) -> list[dict]:
    """Return the Scheduling welcomes among `inbox_items`, each as
    `{"channel_id": hex, "welcome_bytes": bytes}`."""
    out: list[dict] = []
    for it in inbox_items:
        try:
            env = cbor2.loads(bytes.fromhex(it["payload"]))
        except Exception:
            continue
        if not isinstance(env, dict) or env.get("kind") != "welcome":
            continue
        w = cbor2.loads(env["payload"])
        if not isinstance(w, dict) or w.get("channel_type") != "scheduling":
            continue
        out.append({"channel_id": w.get("channel_id"), "welcome_bytes": w.get("welcome_bytes")})
    return out


def wait_scheduling_welcomes(nest, actor, *, at_least: int, timeout: float) -> list[dict]:
    """Poll `actor`'s inbox on `nest` (a dict with `url`) until at least `at_least`
    Scheduling welcomes are present (or `timeout` elapses)."""
    deadline = time.monotonic() + timeout
    found: list[dict] = []
    while time.monotonic() < deadline:
        found = scheduling_welcomes(conv_api.inbox(nest["url"], actor))
        if len(found) >= at_least:
            return found
        time.sleep(2.0)
    return found
