"""How many `fauna.nest.info` reads one actor's hosted-custodian journey draws.

`docs/goal/architecture/security.md` § Transport trust → *The proven identity is
remembered per connection*: every "which nest is this connection bound to" read
on all 7 apps runs through `fauna_client_pair::LinkedNestsMachine::proven_self`,
and a proof costs one `fauna.nest.info` (the cross-check). Before the memo it ran
on EVERY read, so the backups page, the custody leg and the ledger aftermath each
paid one per call, and the tui tripped the release nest's per-actor discovery
budget (`anonymous_rate_limit::DISCOVERY_MAX_EVENTS`: 60 events / 60 s) within a minute of signing
in. The memo serves a passed proof from memory for the rest of that connection.

This file COUNTS the draw instead of reading it out of the code. A dedicated nest
runs with the dispatch-receipt beacon on (`dispatch_core.rs::spawn_dispatch` logs
`ws-rpc dispatch received` with `kind=` and `caller=` at debug), a fresh actor
signs the tui in and drives the hosted-custodian journey through the Backups page,
and the beacon lines carrying `kind=fauna.nest.info` and this actor's hex are
counted from before the sign-in to the journey's last gesture.

A sibling test counts the settings landing's re-loads on one connection, on
whichever app runs it (tui, linux, web): the transport's per-connection
`fauna.nest.info` memo (`fauna_protocol::read_nest_info`, `security.md` §
Transport trust → *The nest's self-description is read once per connection*)
leaves them nothing to read.

**A count per journey, never per minute** (convention 14). The one clock in the
draw is the peer leg's 300 s backstop pass, which can add at most one read to a
journey this short; the bound below leaves room for it.
"""

from __future__ import annotations

import re

import pytest

from helpers.crash_recovery import DISPATCH_BEACON, NestLogWatch

pytestmark = [pytest.mark.tier_3]

# Only the dispatcher's beacon, not the whole nest at debug: the count reads one
# line kind, and a narrow filter keeps the log small.
BEACON_RUST_LOG = "info,fauna_nest::dispatch_core=debug"

# The journey's draw, measured on 2026-10-10 (standalone nest, tui):
# `security.md` § Implementation status today records the same numbers.
# Under the per-connection memo and the settings landing's one read per
# connection: 4, attributed by `_timeline` (each read beside the kinds
# dispatched around it) —
#   * the connection's one reply (beside `fauna.quota.get` and
#     `fauna.conversations.keypackage.count`): the landing load's node,
#     feature-limits and own-limits legs, the barrier's Settings reload, and
#     the sign-in update look (`newest_release`) all share it — the look read
#     its own until 2026-10-10, the fifth read;
#   * the custody leg's identity proof (right after `fauna.auth.nest_handshake`)
#     — a proof is per connection by rule, never shared;
#   * two sync-engine peer-leg passes (`peer_leg::fetch_nest_facts`, right after
#     `fauna.sync.devices.list` / `p2p_participation.set`): the prologue pass at
#     sign-in and one more pass during the Backups journey — one read per pump
#     pass, the ruled relay-URL refresh.
# Before the landing's one read it was 10: three per landing load, and the
# sign-in runs two.
MEASURED_POST_MEMO = 4
# The memo bypassed (`proven_self` proving on every read) added nine identity
# re-proofs on top (measured 19 against the 10 above, the same day): the
# custody leg's second edge, the ledger aftermath, and the Backups page's own
# reads. Against today's 4 that is about 13.
MEASURED_PRE_MEMO = 19
# Above the measured draw with room for one backstop pass and a reconnect (one
# re-proof, one shared re-read, one peer-leg pass), below the memo-bypassed
# draw: a resolver that proves on every read, or a landing that reads per leg
# or per load again, goes red here.
JOURNEY_BOUND = 8


@pytest.fixture
def nest_info_beacon_nest(request, nest_mode, tmp_path_factory):
    """A dedicated claimed nest whose log carries the dispatch beacon.

    Dedicated, never the session nest: the count must see only this test's
    actor, and the beacon's `RUST_LOG` is a start option (`extra_env`, the
    artifact-set IPC kind `crash_nest` uses)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-info-beacon",
        extra_env={"RUST_LOG": BEACON_RUST_LOG})
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def nest_info_beacon_spa_url(static_dir, nest_info_beacon_nest):
    """Function-scoped SPA proxy → ``nest_info_beacon_nest``, for web — reached
    only through ``_login_app_as``'s ``spa_url_fixture``, so a native run never
    builds or serves the SPA."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, nest_info_beacon_nest["url"])
    yield url
    server.shutdown()


def _beacons(lines: list[str], *, kind: str | None = None,
             caller: str | None = None) -> list[str]:
    out = []
    for line in lines:
        if DISPATCH_BEACON not in line:
            continue
        if kind is not None and not re.search(rf"\bkind={re.escape(kind)}(\s|$)", line):
            continue
        if caller is not None and not re.search(rf"\bcaller={caller}(\s|$)", line):
            continue
        out.append(line)
    return out


def _timeline(mine: list[str], *, around: int = 3) -> str:
    """Each `fauna.nest.info` beacon of this actor with the kinds dispatched
    just before and after it — how a read is attributed to its call site (the
    beacon carries no reader, so the neighbours name it)."""
    stamp = re.compile(r"^(\S+)")
    kind = re.compile(r"\bkind=(\S+)")

    def short(line: str) -> str:
        t, k = stamp.search(line), kind.search(line)
        return f"{t.group(1) if t else '?'} {k.group(1) if k else '?'}"

    out, n = [], 0
    for i, line in enumerate(mine):
        if not re.search(r"\bkind=fauna\.nest\.info(\s|$)", line):
            continue
        n += 1
        out.append(f"-- read #{n}")
        lo, hi = max(0, i - around), min(len(mine), i + around + 1)
        out.extend(("  >> " if j == i else "     ") + short(mine[j]) for j in range(lo, hi))
    return "\n".join(out)


@pytest.mark.tui
def test_a_hosted_custodian_journey_draws_a_handful_of_nest_info_reads(
    app, request, nest_info_beacon_nest
):
    """One actor, one sign-in, the hosted-custodian journey: the nest serves a
    handful of `fauna.nest.info` reads to that actor, not one per page read.

    Flow under test::

        arm the beacon watch -> nothing this actor did is before it
        sign in (tui)        -> custody leg, ledger aftermath, update look,
                                peer-leg prologue, settings reads
        Backups: enter, add a custodian, wait for the agent to host it and
        check in, leave and re-enter, remove it, reclaim what it left
        count                -> beacon lines with kind=fauna.nest.info and
                                caller=<this actor>

    The gestures are `test_backups.py::
    test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it`'s,
    without its seeded mail (the count is of identity reads, which no segment
    changes). tui-only: tui is the lead app and its draw is the one measured
    tripping the budget; the resolver it exercises is the shared one all 7 apps
    call.
    """
    from conftest import _login_app_as, _make_user

    nest = nest_info_beacon_nest
    user = _make_user(nest)
    actor_hex = user["actor_id_hex"]

    watch = NestLogWatch(nest)
    _login_app_as(app, request, nest, user, verify_live_actor=True)
    app.backups.require_destination_management_supported()

    app.backups.navigate()
    app.backups.wait_for_destination_count(0)
    app.backups.add_custodian_destination(name="This device", capacity="50 GB")
    app.backups.wait_for_destination_count(1)
    hosted = app.backups.wait_for_custodian_hosting()

    app.driver.navigate_to("feed")
    app.backups.navigate()
    app.backups.wait_for_destination_count(1)
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)
    if app.backups.orphaned_store_visible():
        app.backups.reclaim_orphaned_store()

    lines = watch.lines_since()
    mine = _beacons(lines, caller=actor_hex)
    # Anti-vacuous: a log without this actor's beacons means the debug filter
    # never took, and a zero below would prove nothing.
    assert mine, (
        f"the nest logged no dispatch beacon for actor {actor_hex} across a whole "
        f"sign-in and journey ({len(lines)} log lines since the watch point) — "
        f"RUST_LOG={BEACON_RUST_LOG!r} did not reach the nest, so nothing can be "
        f"counted"
    )
    reads = _beacons(lines, kind="fauna.nest.info", caller=actor_hex)
    anonymous = [
        line for line in _beacons(lines, kind="fauna.nest.info")
        if line not in reads
    ]
    print(
        f"[nest-info-draw] actor={actor_hex[:16]}… fauna.nest.info={len(reads)} "
        f"(other callers: {len(anonymous)}); dispatches by this actor={len(mine)}; "
        f"hosting report={hosted!r}"
    )
    print("[nest-info-draw] each read beside its neighbours:\n" + _timeline(mine))
    assert reads, (
        "the journey drew no fauna.nest.info at all for this actor, but the sign-in "
        "proves the connection's nest identity at least once (the custody leg's "
        "first proof) — the count is not seeing the reads"
    )
    assert len(reads) <= JOURNEY_BOUND, (
        f"one hosted-custodian journey drew {len(reads)} fauna.nest.info reads for "
        f"this actor; the bound is {JOURNEY_BOUND} (measured {MEASURED_POST_MEMO} "
        f"under the per-connection memo and the landing's one read; the memo "
        f"bypassed added nine, {MEASURED_PRE_MEMO} against the pre-landing 10). A "
        f"resolver proving on every read, a landing reading per leg or per load, "
        f"or a new per-read issuer, is back. "
        f"Each read beside its neighbours:\n" + _timeline(mine)
    )


# The settings landing's re-loads on one connection — every app re-reads the
# landing on the nav edge (the quota, the limits and the key packages can change
# between visits), and the transport's per-connection `fauna.nest.info` memo
# (`fauna_protocol::read_nest_info`, `security.md` § Transport trust → *The
# nest's self-description is read once per connection*) serves the node leg, the
# feature-limits rows and the own-limits surface from the read the first load
# made. Read per leg, each re-load drew three.
LANDING_RELOADS = 3
# One stray read is the peer leg's backstop pass (`peer_leg::fetch_nest_facts`,
# one per pump pass), a second a reconnect's one shared re-read; a landing that
# reads per leg draws `3 * LANDING_RELOADS`, one per load `LANDING_RELOADS`.
RELOADS_BOUND = 2


def _wait_for_beacon_count(watch, *, kind: str, caller: str, count: int,
                           timeout: float = 30.0) -> list[str]:
    """Deadline poll until the nest has dispatched `count` `kind` requests from
    `caller` since `watch`'s point — a wait on what the nest received, never
    on a settle-sleep (convention 14)."""
    import time

    deadline = time.monotonic() + timeout
    while True:
        lines = watch.lines_since()
        if len(_beacons(lines, kind=kind, caller=caller)) >= count:
            return lines
        if time.monotonic() >= deadline:
            raise TimeoutError(
                f"the nest dispatched {len(_beacons(lines, kind=kind, caller=caller))} "
                f"{kind} for {caller[:16]}… within {timeout}s, waiting for {count} — "
                f"the settings landing did not re-load on the nav edge"
            )
        time.sleep(0.25)


def test_settings_landing_reloads_draw_no_fresh_nest_info_reads(
    app, request, nest_info_beacon_nest
):
    """One connection, the settings landing loaded and then re-loaded: the
    re-loads draw no `fauna.nest.info` of their own, on whichever app runs it.

    Flow under test::

        sign in, open Settings → Status, wait for the feature-limits section
                             -> the connection's one fauna.nest.info is read
        arm the beacon watch
        feed, Settings → Status, three times; each re-load proven by its own
        fauna.features.status beacon (that read is never memoized)
        count                -> fauna.nest.info beacons for this actor since the
                                watch: the node leg, the feature-limits rows and
                                the own-limits surface all came off the memo

    Not tied to one app: the memo sits in the shared transport (native
    `NestClient`, wasm `WsRpcClient`), so tui, linux and web each run this and
    each count their own draw (`--app linux` / `--app web`).
    """
    from conftest import _login_app_as, _make_user

    nest = nest_info_beacon_nest
    user = _make_user(nest)
    actor_hex = user["actor_id_hex"]

    # web reaches the dedicated nest only through its own SPA proxy; native
    # drivers ignore the fixture name and dial `nest["url"]` directly.
    _login_app_as(app, request, nest, user, verify_live_actor=True,
                  spa_url_fixture="nest_info_beacon_spa_url")
    app.settings._navigate_subpage("status")
    app.settings.wait_for_feature_limits()

    watch = NestLogWatch(nest)
    for n in range(1, LANDING_RELOADS + 1):
        app.driver.navigate_to("feed")
        app.settings._navigate_subpage("status")
        _wait_for_beacon_count(
            watch, kind="fauna.features.status", caller=actor_hex, count=n)
        app.settings.wait_for_feature_limits()

    lines = watch.lines_since()
    mine = _beacons(lines, caller=actor_hex)
    reads = _beacons(lines, kind="fauna.nest.info", caller=actor_hex)
    print(
        f"[nest-info-draw] driver={type(app.driver).__name__} "
        f"landing re-loads={LANDING_RELOADS} fauna.nest.info={len(reads)}; "
        f"dispatches by this actor={len(mine)}"
    )
    print("[nest-info-draw] each read beside its neighbours:\n" + _timeline(mine))
    assert len(reads) <= RELOADS_BOUND, (
        f"{LANDING_RELOADS} settings-landing re-loads on one connection drew "
        f"{len(reads)} fauna.nest.info reads for this actor; the bound is "
        f"{RELOADS_BOUND} (a backstop pass and a reconnect). A landing leg reading "
        f"fauna.nest.info itself instead of through the transport's per-connection "
        f"memo is back. Each read beside its neighbours:\n" + _timeline(mine)
    )
