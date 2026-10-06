"""Nest-trust facet — the v1 capability-settings surface on the Nests page
(docs/goal/ui/nests.md § Trust facet).

Each nest row carries a per-row Now/History lens over the client-authoritative
grant-event log: the grants a nest is *trusted to read* (Now), that nest's
grant-event timeline (History), per-grant renew/revoke, and a
`nest-trust-empty` state when it holds none. Renders the shared trust-enabled
`LinkedNestsMachine`; no trust logic in the client shell (priority #1/#2).

This file covers RENDERING (slice 6 — the home nest row + lens toggle + empty
state on a fresh nest, and the SetLens local flip) and the MINT FLOW (the
scope-first picker, design ratified 2026-07-13: `nest-trust-grant-mint-button`
→ `nest-trust-mint-scope-select` → confirm → the grant renders in the Now lens
and the Mint event in History). The holder half (fetch/open/darken-on-revoke)
stays `test_capability_rescore_drain.py`.

tier_3 (full stack — the trust machine hydrates over the real `fauna-nest`
binary: `fauna.pair.list` + home-nest holder discovery + the grant-log folds; a
mocked backend can't catch flow breaks between the shared machine and the
handlers). Web leads the rendering half; linux leads the mint flow; the other
apps lift the shape (priority #1).
"""
import time
import uuid

import pytest
import requests

from common.auth import register_user
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("nests-and-trust")
def test_nest_trust_facet_renders_empty(ungranted_app):
    """An owner who has granted nothing shows the home nest row with the
    Now/History lens toggle and the `nest-trust-empty` "not trusted to read
    anything" state (honest — not a broken/blank facet).

    `ungranted_app`, not `logged_in_app`: this asserts an EMPTY projection, and
    the shared session `test_user` does not stay empty. `test_backups.py`'s
    destination tests grant that owner's `NestBackupKey`, which a destination
    remove deliberately does not revoke, so the seal backup row (which correctly
    suppresses `nest-trust-empty`) outlives them — see the fixture's docstring.
    """
    app = ungranted_app
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"Nests page not reachable. error: {app.error_text()!r}"
    )
    # The home nest row renders the trust facet (nests.md § Layout — the home
    # nest sits in the same list). Its per-row lens toggle must be present.
    assert app.nest_trust.lens_toggle_present(), (
        f"nest-trust lens toggle missing on the Nests page. error: {app.error_text()!r}"
    )
    # No holder enrolled → no grants → the explicit empty state, NOT a broken or
    # blank facet (and, for a non-admin owner, NOT a hydrate failure — the home
    # row degrades to an empty facet, nests.md § Implementation status).
    assert app.nest_trust.is_empty_state_visible(), (
        f"expected nest-trust-empty for a nest with no grants. error: {app.error_text()!r}"
    )
    assert app.nest_trust.grant_count() == 0, "no grants ⇒ no nest-trust-grant-item rows"


@pytest.mark.feature("nests-and-trust")
def test_nest_trust_lens_toggle_switches_now_history(ungranted_app):
    """Flipping a nest row's lens to History shows the (empty) grant-event
    timeline and hides the Now-only empty state; flipping back to Now restores
    it. Exercises SetLens (local UI state, no nest round-trip).

    `ungranted_app` for the same reason as the test above — the flip-back
    assertion is an empty-state assertion, and so is the mint-affordance one."""
    app = ungranted_app
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible()
    assert app.nest_trust.lens_toggle_present()

    app.nest_trust.show_history()
    # A never-granted nest has an empty History timeline (no rows); the
    # `nest-trust-empty` state is a Now-lens affordance, so it is gone here.
    assert app.nest_trust.history_count() == 0
    assert app.nest_trust.empty_state_absent(), (
        "nest-trust-empty is a Now-lens state; History shows the (empty) timeline"
    )

    app.nest_trust.show_now()
    assert app.nest_trust.is_empty_state_visible(), (
        "flipping back to Now restores the empty-grants state"
    )
    # No enrolled content-processor holder ⇒ an empty mint-option catalog ⇒ the
    # mint affordance stays hidden (never a picker that can only error).
    assert app.nest_trust.mint_button_absent(), (
        "nest-trust-grant-mint-button must be hidden when mint_options is empty"
    )


# ── mint flow (scope-first picker) ───────────────────────────────────────────


@pytest.fixture
def trust_mint_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest for the mint-flow test (same shape as
    test_gated_post_compose.py's `gated_nest` — own nest, own admin, no shared
    session state, so test order stays non-load-bearing; an MSEK/tier can't be
    un-minted on the module-shared nest)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "trust-mint-nest")
    yield nest
    cleanup()


@pytest.mark.feature("nests-and-trust")
def test_nest_trust_mint_paywalled_posts_grant(app, trust_mint_nest):
    """The scope-first mint flow, end-to-end through the UI: an admin author
    creates a tier (the Tiers tab seeds the period-key custody the picker's
    derivability filter reads), then on the Nests page mints a "Serve paywalled
    posts — ‹tier›" trust to the nest's web-serve holder — the
    `monetization.md` § Pillar 2 grant-mint UX. The holder is derived from the
    scope choice (single candidate ⇒ no `nest-trust-mint-holder-select`), the
    grant appears in the Now lens, and the Mint event in History.

    No holder seeding: the nest binary self-enrolls its own web-serve
    content-processor holder at boot (`web_content/holder.rs`, self-approved,
    x25519 attested), so the discoverable holder here is the production one.

    This is the client-UI-equivalent mint that `test_capability_rescore_drain.py`'s
    API-driven fixture mint may cite (testing rule 8's mutation carve-out)."""
    app.nest_trust.require_mint_test_setup_supported()

    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, trust_mint_nest)

    # Login as the nest ADMIN: holder discovery (`list_service_users`) is
    # admin-gated in v1 (nests.md § Known gap), and Admin ⊇ User covers the
    # tier-create + mint. set_state login, test_gated_post_compose.py shape.
    admin = trust_mint_nest["admin"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": trust_mint_nest["url"],
            "secret_hex": bytes(admin["signing_key"]).hex(),
            "handle": "admin",
            "actor_id": admin["actor_id_hex"],
            "device_id": "test-device-trust-mint",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    time.sleep(1.5)

    # ── Author: create the tier through the Tiers tab (custody records the
    # period key client-side — the mint picker's derivability filter reads it).
    tier = f"gold-{uuid.uuid4().hex[:6]}"
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1 My tiers; "
        f"error={subs.error_text()!r}"
    )

    # ── Nests page: the mint affordance is present (an enrolled holder + a
    # held tier ⇒ a non-empty option catalog).
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"Nests page not reachable. error: {app.error_text()!r}"
    )
    assert app.nest_trust.lens_toggle_present()
    assert app.nest_trust.grant_count() == 0, "nothing trusted yet"
    assert app.nest_trust.mint_button_present(), (
        f"mint button missing despite holder + held tier. "
        f"error: {app.error_text()!r}"
    )

    # ── Mint: pick the per-tier paywalled-posts option; the holder is derived
    # (exactly one web-serve candidate), so no holder select renders.
    app.nest_trust.open_mint()
    app.nest_trust.select_scope(app.nest_trust.paywalled_label(tier))
    assert not app.nest_trust.holder_select_visible(), (
        "single holder candidate ⇒ the holder is derived, no "
        "nest-trust-mint-holder-select"
    )
    app.nest_trust.confirm_mint()

    # ── The grant renders in the Now lens with the tier's scope …
    assert app.nest_trust.wait_for_grant_count(1), (
        f"minted grant never rendered in the Now lens. "
        f"error: {app.error_text()!r}"
    )
    scope_text = app.nest_trust.grant_scope_text(0)
    assert tier in scope_text, (
        f"grant row scope should name the tier {tier!r}, got {scope_text!r}"
    )

    # ── … and the Mint event in History.
    app.nest_trust.show_history()
    assert app.nest_trust.history_count() >= 1, "History records the Mint event"


@pytest.mark.feature("nests-and-trust")
def test_backup_trust_rows_render_and_revoke_lands_at_the_destination(
    logged_in_app, second_nest, test_user
):
    """Enrolling a backup destination surfaces BOTH backup trust rows on the
    home nest's row, and the writer row's revoke is spoken to the DESTINATION.

    `nests.md` § Trust facet — backup rows (ratified 2026-07-24): destination
    enroll mints two standing grants, both empowering the source (home) nest —
    the `NestBackupKey` seal grant and, per destination, a writer grant held at
    that destination. Both render on the home nest's row.

    The revoke assertion is the load-bearing one. A writer grant is read and
    revoked over **the destination's own** authenticated connection, never
    through the source nest — that is what keeps the freeze-the-backup
    affordance operable when the source nest is precisely what you are revoking
    because of. Pressing revoke and then seeing the row settle to `missing`
    (the destination answered, and no longer holds a grant for this source)
    proves the call reached the destination's store: a revoke that had been
    routed through the source, or silently dropped, would leave the row
    `active`.

    Drives everything through the client UI (testing.md convention 8): the
    destination is enrolled on the Backups page exactly as a user would, not by
    a raw WS-RPC call standing in for them.
    """
    app = logged_in_app
    app.nest_trust.require_backup_trust_rows_supported()

    # A v1 destination is a nest the owner administers, so the owner's identity
    # must be registered there for the enroll handshake to succeed — the same
    # shape test_backups.py::test_backup_destination_crud uses.
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        pass  # already registered (the session-scoped nest persists across reruns)

    # Start from no destination: every count and row index below is this
    # test's own enrollment, and `test_user` is session-scoped — a destination
    # an earlier module left behind made the wait below read
    # 2.
    app.backups.remove_every_destination()
    # Enroll through the Backups page UI — this is what mints both grants.
    app.backups.add_destination(second_nest["url"], name="Offsite")
    app.backups.wait_for_destination_count(1)

    # Now the Nests page must show both rows on the home nest's row.
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"Nests page not reachable. error: {app.error_text()!r}"
    )
    assert app.nest_trust.wait_for_backup_row_count(2), (
        "expected 2 backup trust rows (the NestBackupKey seal grant + one writer "
        f"row for the enrolled destination), got {app.nest_trust.backup_row_count()}. "
        f"error: {app.error_text()!r}"
    )

    # Row 0 = the seal grant. Its `since` leaf is EMPTY by design — that grant
    # carries no timestamp on the wire (nests.md:67).
    assert S.nests.backup_scope_seal in app.nest_trust.backup_scope_text(0), (
        f"seal row scope was {app.nest_trust.backup_scope_text(0)!r}"
    )
    assert app.nest_trust.backup_status_text(0) == S.nests.backup_status_active
    assert app.nest_trust.backup_since_text(0) == "", (
        "the seal grant carries no granted_at, so nest-trust-backup-since must "
        f"render empty; got {app.nest_trust.backup_since_text(0)!r}"
    )

    # Row 1 = the writer grant at the destination. It DOES carry a timestamp,
    # and it names the destination.
    assert "Offsite" in app.nest_trust.backup_scope_text(1), (
        f"writer row scope was {app.nest_trust.backup_scope_text(1)!r} — it must "
        "name the destination it writes to"
    )
    assert app.nest_trust.backup_status_text(1) == S.nests.backup_status_active, (
        "a freshly enrolled destination holds the source nest's writer grant, so "
        f"the row is active; got {app.nest_trust.backup_status_text(1)!r}"
    )
    assert app.nest_trust.backup_since_text(1) != "", (
        "the writer grant carries granted_at, so its since line must render"
    )
    # The REQUIRED honest-bound copy is present on both rows — revoking freezes
    # only FUTURE writes, and the UI must never imply held custody disappears.
    for i in (0, 1):
        assert app.nest_trust.backup_bound_note_text(i).strip(), (
            f"backup row {i} is missing its required honest-bound copy"
        )

    # `nest-trust-empty` says "this nest is trusted with nothing", so a backup
    # row must suppress it even when the nest holds zero CONTENT grants
    # (nests.md:99). Vacuous if an earlier test in this module already minted a
    # grant for the session-scoped user, hence the guard — but on an isolated
    # run this is the real check, and it is the one that caught linux rendering
    # "not trusted to read anything" directly above "Backs up your messages for
    # you" (fixed 2026-07-24).
    if app.nest_trust.grant_count() == 0:
        assert app.driver.is_absent("nest-trust-empty"), (
            "a home nest holding backup trust rows is plainly trusted with "
            "something, so nest-trust-empty must not render beside them "
            "(nests.md:99)"
        )

    # Revoke the WRITER grant, then assert the destination's own store changed.
    app.nest_trust.revoke_backup(index=1)

    def writer_row_is_missing() -> bool:
        # Re-read the page each poll: the revoke re-hydrates the facet, so the
        # rows are rebuilt. Latency-independent state, not a settle-sleep
        # (testing.md convention 14).
        return app.nest_trust.backup_status_text(1) == S.nests.backup_status_missing

    deadline = time.monotonic() + 20.0
    while time.monotonic() < deadline and not writer_row_is_missing():
        time.sleep(0.5)

    assert writer_row_is_missing(), (
        "after revoking, the destination must report it no longer holds a writer "
        "grant for this source nest — status 'missing', NOT 'active' and NOT "
        "'unreachable'. Still showing "
        f"{app.nest_trust.backup_status_text(1)!r}. A revoke routed through the "
        "source nest (or silently dropped) fails exactly here, which is the "
        "whole reason this row talks to the destination directly. "
        f"error: {app.error_text()!r}"
    )
    # The seal row is untouched — a writer revoke must not freeze sealing.
    assert app.nest_trust.backup_status_text(0) == S.nests.backup_status_active, (
        "revoking one destination's writer grant must not touch the seal grant"
    )

    # Clean up the destination this test enrolled: `second_nest`/`test_user` are
    # session-scoped, shared across every app this test is parametrized over
    # (this test itself doesn't remove the row — a revoke deliberately freezes
    # rather than deletes, nests.md's honest-bound invariant above). Without this,
    # a second client's run of this same test finds the first client's still-
    # configured "Offsite" destination and its own add_destination call lands a
    # SECOND row, so wait_for_destination_count(1) at the top fails with "got 2".
    app.backups.navigate()
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)


@pytest.mark.feature("nests-and-trust")
def test_retained_generations_render_and_restore_lands_at_the_destination(
    logged_in_app, nest_instance, second_nest, test_user
):
    """A superseded backup generation is listed on the trust facet, and its
    restore is spoken to the DESTINATION — the recovery half of the
    rogue-source mitigation.

    `nests.md` § Trust facet — generation recovery (ratified 2026-07-29).
    Revoking a rogue source's writer grant *freezes* the damage; it does not
    undo it. Because the custody writer is the owner's source nest and a
    writer's supersede power is delete power, the destination retains every
    superseded generation for the custody grace window `T`. These rows are what
    the owner can actually roll back to — and until this shell existed, a user
    who revoked had no way to roll anything back at all.

    Flow under test::

        seed mail + enroll → seal grant at source, writer grant at destination
        run-now sweep #1   → segment uploaded, custody recorded at `seg-0…0.dat`
        seed more mail     → the SAME segment grows
        run-now sweep #2   → the same path re-uploads with a NEW manifest, so
                             the first generation is superseded and RETAINED
        Nests page         → `fauna.backup.generation.list` per destination,
                             over the client's OWN connection to it
        press restore      → `fauna.backup.generation.restore` at that same
                             destination, carrying the row's address triple

    **The restore assertion is the load-bearing one, and it is a two-way
    discriminator rather than an assertion by existence.** The outcome copy
    distinguishes the exact two failure modes this surface exists to prevent:
    a restore that actually reached the custody holder reports *restored*,
    while one mis-addressed to the source nest — which answers happily, with
    nothing, per `conformance_backup_generation_client.rs` — comes back
    `NoSuchGeneration` and renders the *past the recovery window* copy instead.
    So a call routed through the source, or silently dropped, fails here with a
    different message rather than passing quietly.

    **Latency-independent throughout** (e2e convention 14). Both sweeps are
    causal barriers, not settle-sleeps: `POST /api/v1/test/backup/run-now` runs
    one sweep synchronously and only then replies. The remaining waits are
    named generous budgets with deadline polls.
    """
    app = logged_in_app
    app.nest_trust.require_retained_generations_supported()

    # The owner must be registered at the destination for the enroll handshake.
    # Idempotent across re-runs of the session-scoped nest.
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        pass

    from tests.test_backups import _seed_one_mail_segment

    def sweep(expect_owners: bool = True) -> None:
        """One synchronous nest-side backup pass — the causal barrier."""
        resp = requests.post(
            f"{nest_instance['url']}/api/v1/test/backup/run-now",
            json={},
            timeout=60,
        )
        assert resp.status_code == 200, (
            f"test-hooks backup run-now returned {resp.status_code}: {resp.text}"
        )
        payload = resp.json()
        assert payload.get("ok") is True, payload
        if expect_owners:
            # The specific diagnostic for "the enroll never reached the nest",
            # separated from a rendering failure so a silent no-op cannot read
            # downstream as a product bug (testing.md point 11).
            assert payload.get("owners_run", 0) >= 1, (
                f"the sweep ran no owners ({payload!r}): this owner lacks either a "
                "granted NestBackupKey or a registered destination, so nothing "
                "could have been uploaded and nothing can be superseded."
            )

    _seed_one_mail_segment(app, nest_instance, test_user)

    # Start from no destination, for the same reason as the test above.
    app.backups.remove_every_destination()
    app.backups.add_destination(second_nest["url"], name="Recovery")
    app.backups.wait_for_destination_count(1)

    # Sweep #1 puts the segment at the destination and records its custody.
    sweep()
    # Growing the SAME segment is what makes the second sweep a supersede
    # rather than a fresh path: the custody row for `<scope>/seg-00000000.dat`
    # is re-recorded with a new manifest hash, and on a reserved (`__mail`) set
    # a supersede RETAINS the displaced generation instead of forgetting it
    # (`message-segment-store.md` § Custody grace window (T)).
    _seed_one_mail_segment(app, nest_instance, test_user)
    sweep()

    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"Nests page not reachable. error: {app.error_text()!r}"
    )
    assert app.nest_trust.wait_for_generation_count(1), (
        "expected at least one retained-generation row after a supersede, got "
        f"{app.nest_trust.generation_count()}. Either the second sweep did not "
        "re-upload the same path (so nothing was superseded), or the facet is not "
        f"reading `fauna.backup.generation.list`. error: {app.error_text()!r}"
    )

    # The row must be a REAL listed generation, not the unreachable stand-in —
    # those are deliberately distinct, and conflating them is the false
    # reassurance this surface exists to prevent (nests.md:122).
    assert app.nest_trust.generation_status_text(0) == S.nests.generation_status_listed, (
        "a reachable destination holding a superseded generation renders "
        f"'listed', got {app.nest_trust.generation_status_text(0)!r} — "
        "'unreachable' here means the client could not ask the destination at all"
    )
    assert app.nest_trust.has_generation_restore(0), (
        "a listed generation must offer the roll-back affordance"
    )
    assert app.nest_trust.generation_path_text(0) != "", (
        "the row must identify what it is a backup of — the plaintext path, or "
        "the path_hash when the custody row carried none (nests.md:123)"
    )
    # The REQUIRED quota-bound copy: retained generations are charged against
    # the owner's storage for the whole window, and this is the surface where
    # that is explicable rather than mysterious (nests.md § Required copy).
    assert "storage" in app.nest_trust.generation_expires_text(0).lower(), (
        "the deadline leaf must carry the required quota-bound copy; got "
        f"{app.nest_trust.generation_expires_text(0)!r}"
    )

    # ── the load-bearing half: the restore lands at the DESTINATION ──
    app.nest_trust.restore_generation(0)

    def restore_reported() -> str:
        # nest-trust-generation-notice (ratified 2026-07-29) — the
        # restore-outcome copy no longer rides error-message; it renders on
        # its own home-row-scoped element instead.
        deadline = time.monotonic() + 20.0
        while time.monotonic() < deadline:
            text = app.nest_trust.generation_notice_text()
            if text.strip():
                return text
            time.sleep(0.5)
        return app.nest_trust.generation_notice_text()

    reported = restore_reported()
    assert S.nests.generation_restored in reported, (
        f"the restore reported {reported!r} on nest-trust-generation-notice "
        f"(error={app.error_text()!r}). The destination holds this generation, "
        "so a call that reached it reports it restored. The specific failure this "
        f"discriminates: {S.nests.generation_past_window!r} means the call was "
        "answered by a nest holding no such custody — i.e. it was addressed to the "
        "SOURCE nest, which answers happily with nothing, rather than to the "
        "destination over the client's own pinned connection."
    )

    # Clean up the destination this test enrolled — `second_nest`/`test_user`
    # are session-scoped and shared across every app this is parametrized
    # over, so a leftover row makes the next client's add land a SECOND one.
    app.backups.navigate()
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)
