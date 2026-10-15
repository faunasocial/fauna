"""Turning hosting on offers to also publish your existing public posts, and
choosing it puts them on your Bluesky account (docs/goal/behavior/
atproto-pds-bridge.md § Projection & backfill; catalog `atproto` 13).

tier_3 (full stack): a real nest, the real Go PDS bridge with its XRPC listener
up, a fake PLC directory and a fake DNS (`helpers/atproto_hosted_world.py`), and
a real app. The user writes a public post through the composer BEFORE hosting,
ticks the minting card's history opt-in, confirms, and the identity's handle
becomes resolvable (the `_atproto` TXT the DNS matrix carries); the earlier
post must then appear in the user's repo on the bridge
(`com.atproto.repo.listRecords`).

It is the opt-in mirror of `test_atproto_firehose_post.py`'s step 6b, which
proves the forward-only DEFAULT keeps such a post off the network. What it
witnesses is the repo, not a relay: the offline `getRepo`-snapshot emit for a
large back-catalogue is still unbuilt (`atproto-pds-bridge.md` § Implementation
status today), and a post above the floor reaches the repo through the
ordinary projection loop.

tui first; the other apps gain their markers in the cross-app lift.
"""
import time
import urllib.error

import pytest

from conftest import get_available_apps
from helpers.atproto_hosted_world import HANDLE_DOMAIN, hosted_world

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

HISTORY_APPS = ("tui",)

# Written before hosting, so it sits below the floor a forward-only consent
# would set — only the opt-in can publish it.
HISTORY_POST = "a public post from before hosting existed"


def _apps(*want: str) -> list[str]:
    available = get_available_apps()
    return [a for a in want if a in available]


@pytest.mark.parametrize("history_app", _apps(*HISTORY_APPS))
@pytest.mark.feature("atproto")
def test_choosing_history_publishes_existing_public_posts(
    history_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    with hosted_world(
        history_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
        projection=True,
    ) as world:
        app = world.app
        app.feed.navigate()
        app.feed.create_post(HISTORY_POST)

        bs = app.atproto_settings
        bs.navigate()
        assert bs.is_page_visible(), f"atproto page unreachable: {app.error_text()!r}"
        assert bs.wait_for_depth_enabled("hosted_visible"), (
            f"hosted must be selectable on a public domain: "
            f"{bs.depth_gate_marker('hosted_visible')!r} err={bs.page_error_text()!r}"
        )
        bs.select_depth("hosted_visible")
        assert bs.wait_for_card(), f"no transition card: {bs.page_error_text()!r}"
        assert not bs.history_backfill_checked(), (
            "the history opt-in must start unticked — forward-only is the default"
        )
        assert bs.set_history_backfill(True), (
            f"ticking the history opt-in must take; label={bs.history_backfill_label()!r} "
            f"err={bs.current_error_text()!r}"
        )
        bs.confirm_transition()
        assert bs.wait_for_depth_level("hosted_visible"), (
            f"confirm failed: level={bs.depth_level()!r} err={bs.page_error_text()!r}"
        )

        did, _user_key = world.await_genesis()
        # The handle becomes resolvable, which opens the first-emit gate.
        world.dns.publish(f"_atproto.alice.{HANDLE_DOMAIN}", [f"did={did}"])

        published: set[str] = set()
        last_error = ""
        # The projection loop's poll backstop is ≤30 s; a green run pays only
        # the real latency (convention 14 — a deadline poll, never a sleep).
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            assert world.proc.poll() is None, "bridge exited:\n" + world.tail()
            try:
                records = world.xrpc_get(
                    "com.atproto.repo.listRecords",
                    f"repo={did}&collection=app.bsky.feed.post&limit=100",
                )["records"]
                published = {r.get("value", {}).get("text", "") for r in records}
            except urllib.error.HTTPError as exc:
                # No repo yet (the gate has not opened) answers an XRPC error.
                last_error = f"HTTP {exc.code}"
            if HISTORY_POST in published:
                break
            time.sleep(1)  # sleep-ok: pacing between poll iterations, not a settle-wait
        assert HISTORY_POST in published, (
            "choosing to publish existing posts must put the pre-hosting post in "
            f"the Bluesky repo; published={published} last_error={last_error!r} "
            f"dns lookups={world.dns.lookup_count()}\nbridge log:\n{world.tail()}"
        )
