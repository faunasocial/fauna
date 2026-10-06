"""Nest-trust facet — a content grant's LIFE after the mint: its honest-bound
copy, revoke, the lapse states and renew, and the mail/calendar mint options
(`docs/goal/ui/nests.md` § Trust facet — grants (Now lens), § Honest bound,
§ Expiry / renewal — first-class states).

`test_nest_trust.py` owns the facet's rendering and the paywalled-posts mint;
these journeys start where that one ends — a grant exists — and drive what the
owner then does with it, all through the app UI (convention 8). The nest-side
half of every gesture is read straight off the nest's own `capability_grants`
table: that row IS what the holder's next `fauna.capabilities.fetch` answers
from, so its absence is the "the nest goes dark on it" observable and its
`epoch_end` the renew observable — no holder process is needed to witness
either (the holder-side shape is `test_capability_rescore_drain.py`'s).

tier_3 (full stack — the shared `LinkedNestsMachine` over the real
`fauna-nest` binary). Each test gets its own nest and logs in as its admin:
holder discovery is admin-gated in v1 (`nests.md` § Implementation status
today), and a grant cannot be un-minted from a shared nest's log.
"""
import sqlite3
import time
import uuid

import pytest

from helpers.nest_trust_setup import login_as_admin, seed_mda_holder
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

_DAY = 24 * 60 * 60

# Named ceilings for UI state that only needs a WS-RPC round trip and a fold —
# deadline polls, never settle-sleeps (convention 14).
_UI_S = 20.0


@pytest.fixture
def trust_grant_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest per test — own admin, own grant table, so the row
    counts below are this test's own (the `trust_mint_nest` shape)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "trust-grant-nest")
    yield nest
    cleanup()


def _login_as_admin(app, nest) -> None:
    """The shared admin sign-in (`helpers.nest_trust_setup.login_as_admin`),
    under this journey's device id."""
    login_as_admin(app, nest, device_id="test-device-trust-grants")


def _grant_rows(nest) -> list[tuple[bytes, bytes, int]]:
    """`(grant_id, holder_pubkey, epoch_end)` for every grant the nest holds
    for its admin — the rows a holder's fetch answers from."""
    owner = bytes.fromhex(nest["admin"]["actor_id_hex"])
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        return conn.execute(
            "SELECT grant_id, holder_pubkey, epoch_end FROM capability_grants"
            " WHERE owner_actor_id = ? ORDER BY created_at",
            (owner,),
        ).fetchall()
    finally:
        conn.close()


def _wait_grant_rows(nest, predicate, what: str, budget_s: float = _UI_S):
    deadline = time.monotonic() + budget_s
    rows = _grant_rows(nest)
    while time.monotonic() < deadline:
        rows = _grant_rows(nest)
        if predicate(rows):
            return rows
        time.sleep(0.5)
    raise AssertionError(f"never observed on the nest: {what}; rows now {rows!r}")


def _create_tier(app, rank: int = 1) -> str:
    """Create a tier on the Tiers tab — what makes a paywalled-posts mint
    option derive. Returns the tier name."""
    tier = f"gold-{uuid.uuid4().hex[:6]}"
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=rank, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} never appeared; error={subs.error_text()!r}"
    )
    return tier


def _mint_paywalled_grant(app, expected_count: int = 1) -> str:
    """Create a tier on the Tiers tab, then mint its paywalled-posts trust to
    the nest's self-enrolled web-serve holder from the Nests page — the
    `test_nest_trust_mint_paywalled_posts_grant` flow, here only as setup — for
    the standard ~90-day window (`nests.md` § Expiry / renewal → *Duration and
    blessing*). Returns the tier name."""
    tier = _create_tier(app)

    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"Nests page not reachable. error: {app.error_text()!r}"
    )
    assert app.nest_trust.mint_button_present(), (
        f"mint button missing despite holder + held tier. error: {app.error_text()!r}"
    )
    app.nest_trust.open_mint()
    app.nest_trust.select_scope(app.nest_trust.paywalled_label(tier))
    app.nest_trust.pick_standard_duration_where_offered()
    app.nest_trust.confirm_mint()
    assert app.nest_trust.wait_for_grant_count(expected_count), (
        f"minted grant never rendered in the Now lens. error: {app.error_text()!r}"
    )
    return tier


@pytest.mark.feature("nests-and-trust")
def test_revoking_a_grant_leaves_now_stays_in_history_and_darkens_the_nest(
    app, trust_grant_nest
):
    """Outcomes 8 and 10: every grant row states what revoking cannot undo,
    and revoking from the row takes the grant out of the Now lens, keeps it in
    History, and deletes it on the nest — so the holder's next fetch goes dark.

    `nests.md` § Trust facet — grants: *Revoke: `CapabilitiesClient::revoke`
    (deletes the `(owner, grant_id)` row; the holder's next fetch goes dark)
    then `grant_log::record_revoke`* — nest first, because revoke narrows.
    """
    app.nest_trust.require_mint_test_setup_supported()
    nest = trust_grant_nest
    _login_as_admin(app, nest)
    tier = _mint_paywalled_grant(app)

    # ── Outcome 10: the REQUIRED honest-bound copy on the CONTENT grant row
    # (the backup rows' twin is asserted in test_nest_trust.py). A post grant
    # is standing-keyed, so the standing wording — never the bounded-mail one.
    note = app.nest_trust.grant_bound_note_text(0)
    assert note == S.nests.bound_note_standing, (
        "a standing-keyed grant row must carry the standing honest-bound copy "
        f"(nests.md § Honest bound); got {note!r}"
    )
    assert app.nest_trust.has_grant_renew(0), "a live grant offers renew"

    # The mint deposited exactly one grant on the nest.
    rows = _wait_grant_rows(nest, lambda r: len(r) == 1, "the minted grant's row")
    minted_id = rows[0][0]

    # ── Outcome 8: revoke from the row.
    app.nest_trust.revoke(0)

    deadline = time.monotonic() + _UI_S
    while time.monotonic() < deadline and app.nest_trust.grant_count() != 0:
        time.sleep(0.5)
    assert app.nest_trust.grant_count() == 0, (
        "a revoked grant leaves the Now lens (current_grants omits it); still "
        f"{app.nest_trust.grant_count()} row(s). error: {app.error_text()!r}"
    )

    # The nest no longer holds it — the row a holder fetch reads is gone.
    _wait_grant_rows(
        nest,
        lambda r: all(row[0] != minted_id for row in r),
        "the revoked grant's row deleted (the holder's next fetch goes dark)",
    )

    # History keeps it: the Mint AND the Revoke, most recent first.
    app.nest_trust.show_history()
    texts = app.nest_trust.history_texts()
    scope = S.nests.scope_posts_tier(tier=tier)
    assert len(texts) >= 2, f"History must keep the mint and the revoke, got {texts!r}"
    assert texts[0].startswith(
        S.nests.history_revoked(scope=scope, when="").split(" · ")[0]
    ), f"the newest History row must be the revoke, got {texts!r}"
    assert any(
        t.startswith(S.nests.history_minted(scope=scope, when="").split(" · ")[0])
        for t in texts[1:]
    ), f"the mint must stay in History after the revoke, got {texts!r}"


@pytest.mark.feature("nests-and-trust")
def test_a_lapsing_grant_reads_expiring_then_paused_and_renew_recovers_it(
    app, trust_grant_nest
):
    """Outcome 9: a grant about to lapse reads "expiring soon", a lapsed one
    reads "paused — renew to resume" and stays on the page with its renew
    control — never a feature that quietly stopped — and renew bumps the
    window on the nest.

    The window is a ~90-day Rust constant (`DEFAULT_GRANT_WINDOW_SECS`), so the
    lapse is reached by moving the trust facet's RENDER clock
    (`trust_clock`, the `trust_facet_advance_clock` agent command) — convention
    14's fake clock, never a sleep. Only the render clock moves: renew stamps
    its new window from the real clock, exactly as in production.
    """
    app.nest_trust.require_mint_test_setup_supported()
    app.nest_trust.require_trust_clock_supported()
    nest = trust_grant_nest
    _login_as_admin(app, nest)
    app.nest_trust.advance_trust_clock(0)
    tier = _mint_paywalled_grant(app)

    assert app.nest_trust.wait_for_grant_status(S.nests.status_active) == (
        S.nests.status_active
    ), "a freshly minted grant is active"
    (_, _, epoch_end_before), = _wait_grant_rows(
        nest, lambda r: len(r) == 1, "the minted grant's row"
    )

    try:
        # Inside the renew-ahead threshold (14 d before a ~90 d window ends).
        app.nest_trust.advance_trust_clock(80 * _DAY)
        seen = app.nest_trust.wait_for_grant_status(S.nests.status_expiring)
        assert seen == S.nests.status_expiring, (
            f"80 days in, the grant must read expiring soon; got {seen!r}. "
            f"error: {app.error_text()!r}"
        )

        # Past the window: paused, still listed, renew offered.
        app.nest_trust.advance_trust_clock(91 * _DAY)
        seen = app.nest_trust.wait_for_grant_status(S.nests.status_expired)
        assert seen == S.nests.status_expired, (
            f"past its window the grant must read {S.nests.status_expired!r} — "
            f"paused work, never silent feature loss; got {seen!r}"
        )
        assert app.nest_trust.grant_count() == 1, (
            "an expired grant stays in the Now lens so the user can renew it"
        )
        assert app.nest_trust.has_grant_renew(0), (
            "an expired grant must offer renew — it is the recovery"
        )

        # Renew from the lapsed row.
        app.nest_trust.renew(0)
        _wait_grant_rows(
            nest,
            lambda r: len(r) == 1 and r[0][2] > epoch_end_before,
            "renew bumped the grant's window on the nest",
        )
    finally:
        app.nest_trust.advance_trust_clock(0)

    # Back at the real clock the renewed grant is active, and History records
    # the renew beside the mint.
    seen = app.nest_trust.wait_for_grant_status(S.nests.status_active)
    assert seen == S.nests.status_active, f"the renewed grant reads {seen!r}"
    app.nest_trust.show_history()
    texts = app.nest_trust.history_texts()
    scope = S.nests.scope_posts_tier(tier=tier)
    assert texts and texts[0].startswith(
        S.nests.history_renewed(scope=scope, when="").split(" · ")[0]
    ), f"the newest History row must be the renew, got {texts!r}"


def _seed_mda_holder(nest) -> bytes:
    """The shared `mda` holder enrollment (`helpers.nest_trust_setup.seed_mda_holder`)."""
    return seed_mda_holder(nest)


@pytest.mark.feature("nests-and-trust")
def test_the_picker_mints_mail_and_calendar_trust_to_the_mail_holder(
    app, trust_grant_nest
):
    """Outcome 12: from the same scope-first picker the owner lets a nest read
    and filter their mail, or read their calendar.

    `nests.md` § Trust facet — grants: "Read and filter my mail" is
    `content.read{mail}` **bundled with** the keyless `content.label-write` —
    never `read{mail}` alone — and "Read my calendar" is `content.read{calendar}`;
    both target the `mda` holder, derived from the scope choice. Both are
    offered only when they would actually mint: mail enabled (the MSEK the
    payload derives from) and an `mda` holder enrolled — this test sets up
    both, the first through the Mail settings page.
    """
    app.nest_trust.require_mint_test_setup_supported()
    app.mail_settings.require_scripted_mua_seed_supported()
    nest = trust_grant_nest
    _login_as_admin(app, nest)

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    holder_x25519 = _seed_mda_holder(nest)

    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible()
    assert app.nest_trust.mint_button_present(), (
        "mail enabled + an mda holder enrolled ⇒ the mail and calendar options "
        f"derive, so the mint control shows. error: {app.error_text()!r}"
    )

    # ── Mail: one gesture, two scopes (the production MDA grant shape).
    app.nest_trust.open_mint()
    app.nest_trust.select_scope(app.nest_trust.MAIL_LABEL)
    assert not app.nest_trust.holder_select_visible(), (
        "one mda candidate ⇒ the holder is derived, no holder select"
    )
    app.nest_trust.confirm_mint()
    assert app.nest_trust.wait_for_grant_count(1), (
        f"the mail grant never rendered. error: {app.error_text()!r}"
    )
    mail_scope = app.nest_trust.grant_scope_text(0)
    assert S.nests.scope_mail in mail_scope and S.nests.scope_spam_labels in mail_scope, (
        "the mail option must mint read{mail} bundled with label-write — the row "
        f"names both; got {mail_scope!r}"
    )

    # ── Calendar.
    app.nest_trust.open_mint()
    app.nest_trust.select_scope(app.nest_trust.CALENDAR_LABEL)
    app.nest_trust.confirm_mint()
    assert app.nest_trust.wait_for_grant_count(2), (
        f"the calendar grant never rendered. error: {app.error_text()!r}"
    )
    scopes = [app.nest_trust.grant_scope_text(j) for j in range(2)]
    assert any(
        S.nests.scope_calendar in s and S.nests.scope_mail not in s for s in scopes
    ), f"one row must be the calendar-only grant; got {scopes!r}"

    # Both deposits reached the nest, sealed to the mda holder.
    rows = _wait_grant_rows(nest, lambda r: len(r) == 2, "both grants deposited")
    assert all(row[1] == holder_x25519 for row in rows), (
        "both grants must be held by the mda holder the options derived"
    )


def _grant_index_for_tier(app, tier: str) -> int:
    """The Now-lens row index whose scope names `tier` — rows are ordered
    soonest-expiry first, so a renewal can move them."""
    for j in range(app.nest_trust.grant_count()):
        if tier in app.nest_trust.grant_scope_text(j):
            return j
    raise AssertionError(f"no grant row names tier {tier!r}")


@pytest.mark.feature("nests-and-trust")
def test_a_blessed_nests_trust_renews_itself_and_a_one_off_trust_lasts_hours(
    app, trust_grant_nest
):
    """Outcome 11: the owner chooses a grant's length and which nests they
    bless; a blessed nest's standing trust renews itself, and a one-off trust
    lasts hours and is never renewed.

    `nests.md` § Expiry / renewal → *Duration and blessing*: the picker offers
    *a few hours* / *90 days*, pre-selecting 90 days on a blessed nest and a
    few hours otherwise; the auto-renew loop renews a blessed nest's standing
    grants inside the renew-ahead threshold, and its due decision reads the
    trust facet's render clock — so moving that clock (convention 14's fake
    clock, never a sleep) and reopening the page (the page refresh runs the
    loop) reaches the renewal. The renewed window is stamped from the real
    clock, exactly as in production; its bump is read off the nest's own
    `capability_grants` row.
    """
    app.nest_trust.require_mint_test_setup_supported()
    app.nest_trust.require_trust_clock_supported()
    app.nest_trust.require_duration_and_blessing_supported()
    nest = trust_grant_nest
    _login_as_admin(app, nest)
    app.nest_trust.advance_trust_clock(0)
    one_off_tier = _create_tier(app, rank=1)
    standing_tier = _create_tier(app, rank=2)

    app.linked_nests.navigate()
    assert app.nest_trust.blessed_state() == "off", "a nest starts un-blessed"

    # ── A one-off trust on the un-blessed nest: the picker's default.
    app.nest_trust.open_mint()
    app.nest_trust.select_scope(app.nest_trust.paywalled_label(one_off_tier))
    assert app.nest_trust.duration_selected() == app.nest_trust.ONE_OFF_LABEL, (
        "an un-blessed nest's mint defaults to the few-hours window"
    )
    app.nest_trust.confirm_mint()
    assert app.nest_trust.wait_for_grant_count(1), (
        f"the one-off grant never rendered. error: {app.error_text()!r}"
    )
    (one_off_id, _, one_off_end), = _wait_grant_rows(
        nest, lambda r: len(r) == 1, "the one-off grant's row"
    )
    hours_left = (one_off_end - time.time()) / 3600
    assert 7.5 < hours_left <= 8.1, (
        f"a one-off trust lasts hours (8), got {hours_left:.2f} h on the nest"
    )

    # ── Bless the nest; a new mint now defaults to the standing window.
    app.nest_trust.set_blessed(True)
    app.nest_trust.open_mint()
    app.nest_trust.select_scope(app.nest_trust.paywalled_label(standing_tier))
    assert app.nest_trust.duration_selected() == app.nest_trust.STANDARD_LABEL, (
        "a blessed nest's mint defaults to the 90-day window"
    )
    app.nest_trust.confirm_mint()
    assert app.nest_trust.wait_for_grant_count(2), (
        f"the standing grant never rendered. error: {app.error_text()!r}"
    )
    rows = _wait_grant_rows(nest, lambda r: len(r) == 2, "both grants' rows")
    (standing_id, _, standing_end), = [r for r in rows if r[0] != one_off_id]
    assert (standing_end - time.time()) > 89 * _DAY, "the standing window is ~90 days"

    j = _grant_index_for_tier(app, standing_tier)
    assert app.nest_trust.grant_status_text(j) == S.nests.status_auto_renewing, (
        "a blessed nest's standing trust reads auto-renewing"
    )
    j = _grant_index_for_tier(app, one_off_tier)
    assert app.nest_trust.grant_status_text(j) != S.nests.status_auto_renewing, (
        "a one-off trust is never renewed, so it must not claim to be"
    )

    try:
        # Inside the renew-ahead threshold of the standing grant (and far past
        # the one-off's hours): reopening the page runs the loop.
        app.nest_trust.advance_trust_clock(80 * _DAY)
        deadline = time.monotonic() + _UI_S
        rows = _grant_rows(nest)
        while time.monotonic() < deadline:
            app.linked_nests.navigate()
            rows = _grant_rows(nest)
            if any(r[0] == standing_id and r[2] > standing_end for r in rows):
                break
            time.sleep(0.5)
        by_id = {r[0]: r[2] for r in rows}
        assert by_id.get(standing_id, 0) > standing_end, (
            "the blessed nest's standing trust must renew itself on the nest; "
            f"epoch_end {by_id.get(standing_id)!r} vs minted {standing_end}. "
            f"error: {app.error_text()!r}"
        )
        assert by_id.get(one_off_id) == one_off_end, (
            "a one-off trust is never renewed — it lapses after its hours"
        )
        seen = app.nest_trust.wait_for_grant_status(
            S.nests.status_expired, index=_grant_index_for_tier(app, one_off_tier)
        )
        assert seen == S.nests.status_expired, (
            f"80 days on, the one-off trust reads paused; got {seen!r}"
        )
    finally:
        app.nest_trust.advance_trust_clock(0)

    # History records the loop's renewal beside the mint.
    app.linked_nests.navigate()
    app.nest_trust.show_history()
    texts = app.nest_trust.history_texts()
    renewed = S.nests.history_renewed(
        scope=S.nests.scope_posts_tier(tier=standing_tier), when=""
    ).split(" · ")[0]
    assert any(t.startswith(renewed) for t in texts), (
        f"History must record the automatic renewal, got {texts!r}"
    )


# The mail sealing epoch is a weekly, absolute index
# (`fauna_mls::wrapped_blob::MAIL_SEALING_EPOCH_SECS`, no config surface).
_MAIL_SEALING_EPOCH_SECS = 7 * _DAY


def _grant_blobs(nest) -> list[tuple[bytes, bytes]]:
    """`(grant_id, blob)` for every grant the nest holds for its admin — the
    canonical dag-cbor bytes a holder's fetch answers with."""
    owner = bytes.fromhex(nest["admin"]["actor_id_hex"])
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        return conn.execute(
            "SELECT grant_id, blob FROM capability_grants"
            " WHERE owner_actor_id = ? ORDER BY created_at",
            (owner,),
        ).fetchall()
    finally:
        conn.close()


def _wrap_epochs_and_factors(blob: bytes) -> tuple[list[int], set[str | None]]:
    """The sorted distinct sealing epochs a deposited grant's wraps cover, and
    the set of factors those wraps carry — decoded from the blob's own bytes,
    so the assertion is on what the holder would open, not a client claim."""
    import cbor2

    decoded = cbor2.loads(blob)
    wraps = decoded["wrapped_keys"]
    epochs = sorted({w["epoch"] for w in wraps if w.get("epoch") is not None})
    factors = {w["scope"].get("factor") for w in wraps}
    return epochs, factors


@pytest.mark.tui
@pytest.mark.feature("nests-and-trust")
def test_a_subscribed_labelers_trust_renews_with_its_epoch_keys(
    app, trust_grant_nest, run_seal_helper
):
    """Outcome 9, for the bounded per-labeler trust: a subscribed mail
    labeler's trust lapses like any other and renews from its row — and the
    renewal carries the epoch keys the extended window needs, so the labeler
    does not go dark at 90 days until the user re-subscribes.

    `nests.md` § Expiry / renewal → *Duration and blessing*: a bounded mail
    grant renews with the epoch wraps for the window it extends into, sealed
    to the holder the live roster names, each confined to the grant's own
    labeler; the nest refuses a bounded extension that leaves an epoch
    uncovered. The lapse is reached by moving the trust facet's RENDER clock
    (convention 14); the renew stamps its window from the real clock, so the
    new end lands seconds past the old one and a real sealing-epoch crossing
    cannot be forced from here — the exact extension-epoch wraps are pinned in
    `fauna-client-pair`'s `a_bounded_mail_grant_renews_with_its_epoch_wraps`
    under a fake clock. What this journey witnesses on the nest's own row: the
    renew is no longer refused, `epoch_end` moves, the blob stays per-epoch
    and labeler-confined, and its wraps cover every sealing epoch through the
    new end (a dropped key set would leave the new end's epoch uncovered
    whenever the boundary is crossed, and the nest would refuse the bump).

    tui only: the lead app builds the grant-wired catalog machine that mints
    the subscription's grant; the six still build the grant-less one.
    """
    from helpers.labeler_publish import FIXTURES, publish_wasm_labeler

    app.nest_trust.require_mint_test_setup_supported()
    app.nest_trust.require_trust_clock_supported()
    nest = trust_grant_nest
    _login_as_admin(app, nest)
    app.nest_trust.advance_trust_clock(0)

    # Preconditions (convention 8 carve-out (b)): mail enabled for the owner,
    # an `mda` holder to seal to, a published mail labeler.
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    _seed_mda_holder(nest)
    labeler_id = publish_wasm_labeler(
        run_seal_helper, nest, FIXTURES / "cat_labeler.wat", content_kind="mail"
    )
    factor = "labeler:" + labeler_id.hex()
    short = labeler_id.hex()[:12] + "…"

    # Subscribing mints the bounded per-labeler grant (the journey
    # owns that assertion; here it is the grant whose life we drive).
    cat = app.labeler_catalog
    cat.navigate_catalog()
    index = cat.find_index_by_factor(factor)
    assert index is not None, (
        f"published mail labeler {factor!r} not in the catalog; error={app.error_text()!r}"
    )
    cat.subscribe(index)
    assert cat.wait_for_subscribed_state(index, subscribed=True), (
        f"subscribe did not flip the row; error={app.error_text()!r}"
    )
    (grant_id, _, epoch_end_before), = _wait_grant_rows(
        nest, lambda r: len(r) == 1, "the subscription's grant deposited"
    )
    (_, blob_before), = _grant_blobs(nest)
    epochs_before, factors_before = _wrap_epochs_and_factors(blob_before)
    assert factors_before == {factor}, "minted per-epoch and confined to the labeler"
    assert epochs_before and epochs_before[-1] == epoch_end_before // _MAIL_SEALING_EPOCH_SECS, (
        "the mint covers its window through the end's epoch"
    )

    app.linked_nests.navigate()
    assert app.nest_trust.wait_for_grant_count(1), (
        f"the labeler trust never rendered on the Nests page; error={app.error_text()!r}"
    )
    assert short in app.nest_trust.grant_scope_text(0), "the row names the labeler"

    try:
        # Past the window: paused, still listed, renew offered — the bounded
        # trust lapses exactly like a standing one.
        app.nest_trust.advance_trust_clock(91 * _DAY)
        seen = app.nest_trust.wait_for_grant_status(S.nests.status_expired)
        assert seen == S.nests.status_expired, (
            f"past its window the labeler trust must read {S.nests.status_expired!r}; "
            f"got {seen!r}. error: {app.error_text()!r}"
        )
        assert app.nest_trust.has_grant_renew(0), "the lapsed labeler trust offers renew"

        # Renew from the lapsed row: no refusal, and the nest's row moves.
        app.nest_trust.renew(0)
        _wait_grant_rows(
            nest,
            lambda r: len(r) == 1 and r[0][0] == grant_id and r[0][2] > epoch_end_before,
            "the renew bumped the labeler grant's window on the nest",
        )
        assert not app.has_error(), (
            f"a bounded grant's renew must no longer be refused; got {app.error_text()!r}"
        )
    finally:
        app.nest_trust.advance_trust_clock(0)

    # The blob the holder would fetch: still per-epoch, still confined to the
    # labeler, and its wraps cover every sealing epoch through the new end —
    # the keys the extension needed rode the renew.
    (_, _, epoch_end_after), = _grant_rows(nest)
    (_, blob_after), = _grant_blobs(nest)
    epochs_after, factors_after = _wrap_epochs_and_factors(blob_after)
    assert factors_after == {factor}, (
        f"every wrap stays confined to the labeler after the renew: {factors_after!r}"
    )
    assert epochs_after[: len(epochs_before)] == epochs_before, "the held epochs are kept"
    last_epoch = epoch_end_after // _MAIL_SEALING_EPOCH_SECS
    assert epochs_after == list(range(epochs_before[0], last_epoch + 1)), (
        f"the wraps must cover every sealing epoch through the new end "
        f"({epochs_before[0]}..={last_epoch}); got {epochs_after!r}"
    )

    # Back at the real clock the renewed trust is active, and History records
    # the renew naming the labeler.
    seen = app.nest_trust.wait_for_grant_status(S.nests.status_active)
    assert seen == S.nests.status_active, f"the renewed labeler trust reads {seen!r}"
    app.nest_trust.show_history()
    texts = app.nest_trust.history_texts()
    renewed_prefix = S.nests.history_renewed(scope="", when="").split(" · ")[0]
    assert texts and texts[0].startswith(renewed_prefix) and short in texts[0], (
        f"the newest History row must be this labeler's renew, got {texts!r}"
    )
