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
    cid = re.compile(r"\bcorrelation_id=(\d+)")

    def short(line: str) -> str:
        t, k, c = stamp.search(line), kind.search(line), cid.search(line)
        return (f"{t.group(1) if t else '?'} #{c.group(1) if c else '?'} "
                f"{k.group(1) if k else '?'}")

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


# The admin dashboard's re-entries on one connection — every app re-reads the
# dashboard's card set on each arrival (`AdminClient::dashboard_snapshot`; the
# storage, member and connection cards change between visits), and its one
# `fauna.nest.info` fold (the domain card) comes off the transport's
# per-connection memo. Read per entry, an admin moving between pages drew one of
# its 60-a-minute discovery budget on every arrival: a 2026-10-28 docker run shed
# the session admin's `fauna.nest.info` in bursts of up to 96 while the linux
# app re-entered the admin shell, and the harness's own read in
# `test_admin.py::test_admin_dashboard_shows_the_one_card_set` was refused.
DASHBOARD_REENTRIES = 3


def test_admin_dashboard_reentries_draw_no_fresh_nest_info_reads(
    app, request, nest_info_beacon_nest
):
    """One admin connection, the dashboard loaded and then re-entered: the
    re-entries draw no `fauna.nest.info` of their own, on whichever app runs it.

    Flow under test::

        sign in as the nest's admin, open the dashboard, wait for its cards
                             -> the connection's one fauna.nest.info is read
        arm the beacon watch
        feed, admin dashboard, three times; each re-entry proven by its own
        fauna.admin.stats beacon (that read is never memoized)
        count                -> fauna.nest.info beacons for the admin since the
                                watch: the domain card came off the memo

    Not tied to one app: the card set is the shared `dashboard_snapshot`, so
    tui, linux and web each run this and count their own draw.
    """
    from conftest import _login_admin_as

    nest = nest_info_beacon_nest
    admin_hex = bytes(nest["admin"]["signing_key"].verify_key).hex()

    _login_admin_as(app, request, nest, spa_url_fixture="nest_info_beacon_spa_url",
                    fixture_name="nest_info_beacon_admin_app")
    app.admin.navigate_dashboard()

    watch = NestLogWatch(nest)
    for n in range(1, DASHBOARD_REENTRIES + 1):
        app.driver.navigate_to("feed")
        app.admin.navigate_dashboard()
        _wait_for_beacon_count(
            watch, kind="fauna.admin.stats", caller=admin_hex, count=n)

    lines = watch.lines_since()
    mine = _beacons(lines, caller=admin_hex)
    reads = _beacons(lines, kind="fauna.nest.info", caller=admin_hex)
    stats = _beacons(lines, kind="fauna.admin.stats", caller=admin_hex)
    print(
        f"[nest-info-draw] driver={type(app.driver).__name__} "
        f"dashboard re-entries={DASHBOARD_REENTRIES} fauna.admin.stats={len(stats)} "
        f"fauna.nest.info={len(reads)}; dispatches by the admin={len(mine)}"
    )
    print("[nest-info-draw] each read beside its neighbours:\n" + _timeline(mine))
    assert len(reads) <= RELOADS_BOUND, (
        f"{DASHBOARD_REENTRIES} admin-dashboard re-entries on one connection "
        f"({len(stats)} card-set reads) drew {len(reads)} fauna.nest.info reads "
        f"for the admin; the bound is {RELOADS_BOUND} (a backstop pass and a "
        f"reconnect). The dashboard's domain card reading fauna.nest.info itself "
        f"instead of through the transport's per-connection memo is back. Each "
        f"read beside its neighbours:\n" + _timeline(mine)
    )



# The session user's pages on one connection — Backups, the atproto settings
# page, Conversations — each re-entered on the nav edge. None of them may read
# `fauna.nest.info` per render: every user-page reader whose answer is fixed for
# the connection (the capability checks in the subscriptions and mail-settings
# seams, the follow intent's own-nest id) comes off the transport's
# per-connection memo (`fauna_protocol::read_nest_info`, `security.md` §
# Transport trust → *The nest's self-description is read once per
# connection*). Measured 2026-11-02 on linux: three entries of each page drew 0.
USER_PAGE_REENTRIES = 3


def test_user_page_reentries_draw_no_fresh_nest_info_reads(
    app, request, nest_info_beacon_nest
):
    """One user connection, Backups / atproto settings / Conversations each
    entered three times: the re-entries draw no `fauna.nest.info` of their own,
    on whichever app runs it.

    Flow under test::

        sign in, open Backups once -> the connection's reads are made
        arm the beacon watch
        per page, three times: feed, then the page, waiting on its own rendered
        state (Backups' destination list, the atproto landmark, the new-
        conversation button); Backups' re-entries are proven by its own
        fauna.filesync.snapshot.list_restore_history beacon (never memoized)
        count                -> fauna.nest.info beacons for this actor since the
                                watch

    Not tied to one app: the readers sit in shared Rust, so tui, linux and web
    each run this and count their own draw.
    """
    from conftest import _login_app_as, _make_user

    nest = nest_info_beacon_nest
    user = _make_user(nest)
    actor_hex = user["actor_id_hex"]

    _login_app_as(app, request, nest, user, verify_live_actor=True,
                  spa_url_fixture="nest_info_beacon_spa_url")
    app.backups.navigate()
    app.backups.wait_for_destination_count(0)

    watch = NestLogWatch(nest)
    for n in range(1, USER_PAGE_REENTRIES + 1):
        app.driver.navigate_to("feed")
        app.backups.navigate()
        app.backups.wait_for_destination_count(0)
        _wait_for_beacon_count(
            watch, kind="fauna.filesync.snapshot.list_restore_history",
            caller=actor_hex, count=n)
    for _ in range(USER_PAGE_REENTRIES):
        app.driver.navigate_to("feed")
        app.atproto_settings.navigate()
        assert app.atproto_settings.is_page_visible(), (
            f"the atproto page did not render on re-entry: error="
            f"{app.error_text()!r}")
    for _ in range(USER_PAGE_REENTRIES):
        app.driver.navigate_to("feed")
        app.conversations.navigate()

    lines = watch.lines_since()
    mine = _beacons(lines, caller=actor_hex)
    reads = _beacons(lines, kind="fauna.nest.info", caller=actor_hex)
    print(
        f"[nest-info-draw] driver={type(app.driver).__name__} user-page "
        f"re-entries={3 * USER_PAGE_REENTRIES} fauna.nest.info={len(reads)}; "
        f"dispatches by this actor={len(mine)}"
    )
    print("[nest-info-draw] each read beside its neighbours:\n" + _timeline(mine))
    assert len(reads) <= RELOADS_BOUND, (
        f"{3 * USER_PAGE_REENTRIES} user-page re-entries (Backups, atproto "
        f"settings, Conversations) on one connection drew {len(reads)} "
        f"fauna.nest.info reads for this actor; the bound is {RELOADS_BOUND} (a "
        f"backstop pass and a reconnect). A user-page reader reading "
        f"fauna.nest.info itself instead of through the transport's "
        f"per-connection memo is back. Each read beside its neighbours:\n"
        + _timeline(mine)
    )


# One app launch as a signed-in user, then the folders page down to an expanded
# folder row — what every `test_folders.py` test costs the session user, since
# each test relaunches the app (`logged_in_app`). The discovery budget is 60 a
# minute per actor and kind; a module of short tests launches the session user
# about fifteen times a minute, so the per-launch draw is what has to fit.
# 3 on tui and linux: linux drew 4 while a launch's first reader (archive
# import's capability probe) read before the connection was numbered and the
# memo remembered nothing; `read_nest_info` now tickets on the ready
# connection, so that read is remembered for it.
LAUNCH_BOUND = 3


def test_one_launch_and_the_folders_page_draw_a_bounded_nest_info_count(
    app, request, nest_info_beacon_nest
):
    """One sign-in, the folders page, one folder created and expanded: the nest
    serves a bounded number of `fauna.nest.info` reads to that actor.

    Flow under test::

        arm the beacon watch
        sign in               -> the connection's one shared read, the
                                 identity proof, the peer-leg prologue pass
        Folders: create a folder through the wizard, expand its row
                              -> the destination-places read proves the bound
                                 identity (memoized on this connection)
        count                 -> fauna.nest.info beacons for this actor

    Not tied to one app: tui and linux each run it and count their own draw.
    """
    import secrets as _secrets

    from conftest import _login_app_as, _make_user
    from helpers.budgets import RPC_ROUNDTRIP_S
    from helpers.waiting import wait_until

    nest = nest_info_beacon_nest
    user = _make_user(nest)
    actor_hex = user["actor_id_hex"]

    watch = NestLogWatch(nest)
    _login_app_as(app, request, nest, user, verify_live_actor=True,
                  spa_url_fixture="nest_info_beacon_spa_url")
    b = app.backups
    name = f"draw-set-{_secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)
    wait_until(b.share_button_visible, RPC_ROUNDTRIP_S,
               diagnose=lambda: f"error={app.error_text()!r}")

    lines = watch.lines_since()
    mine = _beacons(lines, caller=actor_hex)
    assert mine, (
        f"no dispatch beacon for actor {actor_hex} — RUST_LOG={BEACON_RUST_LOG!r} "
        f"did not reach the nest, so nothing can be counted"
    )
    reads = _beacons(lines, kind="fauna.nest.info", caller=actor_hex)
    print(
        f"[nest-info-draw] driver={type(app.driver).__name__} launch+folders "
        f"fauna.nest.info={len(reads)}; dispatches by this actor={len(mine)}"
    )
    print("[nest-info-draw] each read beside its neighbours:\n" + _timeline(mine))
    assert len(reads) <= LAUNCH_BOUND, (
        f"one launch and the folders page drew {len(reads)} fauna.nest.info reads "
        f"for this actor; the bound is {LAUNCH_BOUND}. Each read beside its "
        f"neighbours:\n" + _timeline(mine)
    )


# A sign-in's own draw — the burst every app fires when it signs in (the
# prefetches, the settings and Backups landings, the pairings list, the
# custody leg, the ledger aftermath). The rule (`transport-trust.md` §
# Transport trust → *The nest's self-description is read once per
# connection*) allows the sign-in connection two: the memo's one read, and the
# identity cross-check's own (*The proven identity is remembered per
# connection*). On top of those the sync engine's peer-leg prologue reads on a
# connection of its own (`peer_leg::fetch_nest_facts`, one per pump pass, the
# ruled relay-URL refresh) and a backstop pass can add one more.
SIGN_IN_BOUND = 4


def test_a_sign_in_draws_the_connection_read_and_the_cross_check_only(
    app, request, nest_info_beacon_nest
):
    """One actor signs in and lands on Settings → Status and Backups: the nest
    serves that actor the connection's one memoised read, the identity
    cross-check and the peer leg's own — never a reader of its own beside them,
    on whichever app runs it.

    Flow under test::

        arm the beacon watch -> nothing this actor did is before it
        sign in              -> the sign-in burst
        Settings → Status, wait for the feature-limits section; Backups, wait
        for its destination list -> the landings' reads are made
        count                -> fauna.nest.info beacons for this actor since
                                the watch

    Measured 2026-11-02 with the dispatch beacon: linux's
    sign-in connection alone read five, tui's two. The same day, after the
    gap-read ticket, the shared proof gate, the home row's claim-free address
    and the web page's visit-only hydrate: linux 2 (the memoised read and the
    cross-check), tui 3 (those two and the peer leg's prologue on its own
    connection). Not tied to one app: tui, linux and web each run it.
    """
    from conftest import _login_app_as, _make_user

    nest = nest_info_beacon_nest
    user = _make_user(nest)
    actor_hex = user["actor_id_hex"]

    watch = NestLogWatch(nest)
    _login_app_as(app, request, nest, user, verify_live_actor=True,
                  spa_url_fixture="nest_info_beacon_spa_url")
    app.settings._navigate_subpage("status")
    app.settings.wait_for_feature_limits()
    app.backups.navigate()
    app.backups.wait_for_destination_count(0)

    lines = watch.lines_since()
    mine = _beacons(lines, caller=actor_hex)
    assert mine, (
        f"the nest logged no dispatch beacon for actor {actor_hex} across a whole "
        f"sign-in ({len(lines)} log lines since the watch point) — "
        f"RUST_LOG={BEACON_RUST_LOG!r} did not reach the nest, so nothing can be "
        f"counted"
    )
    reads = _beacons(lines, kind="fauna.nest.info", caller=actor_hex)
    print(
        f"[nest-info-draw] driver={type(app.driver).__name__} sign-in "
        f"fauna.nest.info={len(reads)}; dispatches by this actor={len(mine)}"
    )
    print("[nest-info-draw] each read beside its neighbours:\n" + _timeline(mine))
    assert len(reads) <= SIGN_IN_BOUND, (
        f"one sign-in drew {len(reads)} fauna.nest.info reads for this actor; the "
        f"bound is {SIGN_IN_BOUND} (the connection's memoised read, the identity "
        f"cross-check, the peer leg's prologue and one backstop pass). A sign-in "
        f"reader fetching fauna.nest.info itself instead of through "
        f"`fauna_protocol::read_nest_info` is back. Each read beside its "
        f"neighbours:\n" + _timeline(mine)
    )
