"""Feed bridge-subscribe selector is gated on the nest's supported bridges.

`version-compatibility.md` § Dimension 3 (capability consumption): a client must
not offer a bridge protocol the nest's build can't serve. The feed-subscribe
affordance (`bridge-feed-subscribe-toggle`, which opens the `bridge-form-*` form)
is driven by the shared `FeedManager.refresh_available_bridges` →
`FeedSnapshot.available_bridges`, projected from the **server-filtered**
`fauna.bridges.list` (cfg-gated registration + per-bridge runtime `available`).

History (the marker this test used to carry, and why it's gone):

  * `build_node()` (`tests/common/nest.py`) builds the e2e nest with
    `--features test-hooks,nostr` since 2026-06-19 ("unblock nostr
    e2e") — so the `NostrProvider` is registered (`bins/fauna-nest/src/lib.rs`).
  * Originally (through the plaintext/encrypted storage-mode era): the shared
    `logged_in_app` fixture committed plaintext storage, and nostr's runtime
    `available` flag was exactly `storage_mode == Plaintext`
    (`nostr::nostr_bridging_available`) — so `fauna.bridges.list` surfaced
    nostr, `available_bridges` was non-empty, and the affordance was
    **correctly shown** — making the "nest supports no bridges" premise this
    test needs unreachable on the shared e2e nest (confirmed `--client macos`,
    2026-06-28 N+39). That change (adding nostr to `build_node`) had silently
    regressed this test on every app; it was green when authored
    (2026-06-16, when `build_node` was `--features test-hooks` only).
  * No-modes retirement (ratified 2026-07-12): `logged_in_app` no longer
    commits any storage mode. `nostr_bridging_available` gated on the LEGACY
    `nest_mode` row briefly, which nothing wrote
    any more — then migrated (S8.9) to `db::any_nsec_deposited`
    (`bins/fauna-nest/src/nostr/mod.rs:68-74`, pinned by
    `conformance_nostr_storage_gate.rs::legacy_plaintext_row_alone_no_longer_
    opens_the_gate`): a fresh nest with no deposited nsec is unconditionally
    `!available`, for a different reason than the docstring here used to say —
    the *premise* (no bridges available) still reliably holds on a clean
    session, it just isn't a static fact worth hard-coding into a marker.
  * Graded 2026-08-24: a detached tui run XPASSed
    (`test_bridge_subscribe_toggle_hidden_when_nest_supports_no_bridges[tui]`),
    and `tests/e2e-unified/baselines/apple-baseline.json` independently records
    `xpassed` for both `[macos]` and `[ios]`. Rather than trust any of that as a
    permanent fact (the premise depends on session-nest history — see below),
    the test now checks it live every run and skips cleanly when unmet, so no
    marker is needed either way.

**The premise is checked live, not assumed.** `test_user`/`nest_instance` are
session-scoped (`conftest.py`), so this test's actor is the SAME actor other
tests in the same run use — including
`test_feed_bridge_subscribe.py::test_bridge_subscribe_and_unsubscribe_round_trip`,
which deposits an nsec (`app.nostr.link_generate()`) to make a bridge
available and undoes it in a `finally`. A crash between those two steps (or
any other bridge-linking side effect landing on this shared actor) would
leave the nest genuinely serving bridges, and this test's premise wouldn't
hold — asserting the toggle hidden would then be testing a *different*
nest state than the one described. So this test reads `fauna.bridges.list`
straight from the nest (bypassing the client's own cache) before asserting,
and skips — rather than falsely passing or failing — when the premise isn't
met.

**The settle wait below is NOT yet a causal anchor (convention 14 debt,
tracked, not silently accepted).** `FeedManager`'s `refresh_available_bridges`
result isn't published anywhere in the client's serialized e2e state
(`apps/fauna-tui/src/feed/mod.rs::state_json` exposes `posts` only), and tui's
`barrier` mechanism (`drain_pending_ui_messages`) only drains UI-thread work
**already enqueued** at the moment it runs — it cannot wait for an in-flight
network round trip's `UiMessage` to land later
(`apps/fauna-tui/src/automation.rs:1080-1116`). The three refreshes
(`refresh_feeds` → `refresh_bridge_feeds` → `refresh_available_bridges`) are
sequential awaits in one spawned task
(`apps/fauna-tui/src/feed/mod.rs:445-455`), so once `feed-view` is visible the
later two are causally *certain* to run next — but nothing observable marks
when the last one has actually finished. Per this convention's own corollary
("if the product schedules it asynchronously with no initiation marker, that
is a product-side restructure, not a longer sleep"), the honest fix is a new
completion observable in `FeedSnapshot`/`state_json` (and its 7-app
equivalents) — out of scope here as a standalone slice; tracked as follow-on
work, not silently declared `# sleep-ok:` (that annotation is for genuinely
elapsed-time residue or poll cadence, and this is neither — it's a missing
signal).
"""
import time

from actions.api_actor import ApiActor
import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("bridges")
def test_bridge_subscribe_toggle_hidden_when_nest_supports_no_bridges(
    logged_in_app, nest_instance, test_user
):
    """Against a nest currently serving no bridges, the bridge-feed subscribe
    affordance is hidden — the client reads the nest's (empty) available-bridge
    set and offers nothing it can't serve."""
    app = logged_in_app

    # Ground truth from the nest itself (fauna.bridges.list), not the client's
    # own cache — see module docstring: the shared session actor may carry a
    # bridge link left by a sibling test. `fauna.bridges.list` lists every
    # cfg-registered provider (nostr is always present — the e2e nest builds
    # with `--features nostr`), each carrying its own runtime `available`
    # flag; `FeedManager.refresh_available_bridges` filters on THAT flag
    # (`libs/fauna-feed/src/manager.rs:782`, `.filter(|b| b.available)`), not
    # on list membership — so the premise check must filter the same way.
    actor = ApiActor(
        nest_instance["url"], test_user["token"], test_user["actor_id_hex"],
        bytes(test_user["signing_key"]),
    )
    available = [b for b in actor.bridges_list() if b.get("available")]
    if available:
        pytest.skip(
            f"premise unreachable this run: fauna.bridges.list has an "
            f"available bridge ({[b.get('id') for b in available]!r}) — a "
            f"sibling test left one linked on the shared session actor"
        )

    # Land on the feed page and let it run its initial loads (which include
    # `refresh_available_bridges`, populating snapshot.available_bridges = []).
    app.driver.wait_for("feed-view")
    # Not yet a causal anchor — see module docstring's "settle wait" section.
    time.sleep(1)

    assert app.driver.is_absent("bridge-feed-subscribe-toggle"), (
        "the bridge-feed-subscribe affordance must be hidden when the nest "
        "supports no bridges (fauna.bridges.list empty) — a client must not "
        "offer a protocol the nest can't serve (version-compatibility.md § Dim 3)"
    )
