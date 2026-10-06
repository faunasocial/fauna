"""The feature plane's transparency surface, end to end against a real nest.

`docs/goal/architecture/dynamic-features.md` § Transparency & auditability and
§ What this is NOT boundary 4: *"No silent gates. Every active restriction is
visible to the person it binds — which feature, what limit, which tier set
it."* These tests are that invariant's e2e half — the nest really answers
`fauna.features.status`, the app really folds it, and the three things boundary
4 names really reach the screen.

Two layers, deliberately: the tier-1 **structural** bounds every build ships,
and an **admin-authored** policy written through `fauna.features.policy.update`.
The second is the one that proves attribution is real — a screen that hard-coded
"Fauna's built-in limits" passes every tier-1 assertion here and fails the admin
one, which is exactly the difference between rendering a number and rendering
*who set it*.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

# The guardian arm drives `test_family.py`'s guardian + ward pair. Importing the
# two fixtures registers them in this module; `family_pair` resets the ward's
# policy document (its `features` sub-document included) before each test.
from tests.test_family import _shared_family, family_pair  # noqa: F401

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _require_surface(app) -> None:
    """tui shipped this surface first (2026-08-11); linux/web/android/macos/ios
    landed their legs of the six-app trickle-down; windows closed the set
    2026-08-26."""
    if not (
        app.driver.is_tui()
        or app.driver.is_linux()
        or app.driver.is_web()
        or app.driver.is_android()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_windows()
    ):
        skip_unbuilt(
            app.driver,
            surface="feature-limits-section",
            detail="the feature plane's transparency screen; the shared "
            "derivations (fauna-client-features + its UniFFI/wasm faces) are "
            "landed, so the remaining per-app work is a lift, not a build",
            tracked="",
        )


def _open(app):
    _require_surface(app)
    app.settings._navigate_subpage("status")
    try:
        app.settings.wait_for_feature_limits()
    except TimeoutError:
        raise AssertionError(
            "feature-limits-section never rendered — fauna.features.status or "
            "fauna.nest.info may not have resolved: "
            f"{app.driver.diagnose('feature-limits-section')} "
            f"error={app.error_text()!r}"
        ) from None


@pytest.mark.feature("status-quotas-and-limits")
def test_feature_limits_section_names_the_gated_members(logged_in_app):
    """The surface exists, and it answers for members the user is NOT limited on.

    "Unrestricted" is an answer: a reply that omitted an unbounded member could
    not be told apart from a nest that never heard of the feature, so every
    registry member the build carries gets a row. On a default nest that is all
    three — `payments` is compiled in, so its `subscriptions` capability token
    is advertised and its row is not hidden.
    """
    _open(logged_in_app)

    rows = [logged_in_app.settings.feature_limit_row(i) for i in range(3)]
    names = [r["name"] for r in rows]
    assert names == [
        S.features.name_payments,
        S.features.name_zaps,
        S.features.name_p2p_share,
    ], f"expected every registry member in registry order, got {names}"

    # Nothing is spent on a fresh account, so every member is actionable — the
    # bounded-but-unspent state, which is the one a client that keyed off
    # `availability` instead of the affordance would wrongly call restricted.
    for row in rows:
        assert row["status"] == S.features.status_available, row
        assert "restriction" not in row, (
            f"{row['name']} painted a why-line with nothing blocking: {row}"
        )


@pytest.mark.feature("status-quotas-and-limits")
def test_a_quota_cell_shows_the_limit_the_headroom_and_the_binding_tier(
    logged_in_app,
):
    """Boundary 4's three requirements, on one real cell from a real nest.

    Read scoped to its own row and its own cell — several members are on screen
    at once, and a flat query would answer with whichever feature happened to
    register first (`testing.md` point 1).
    """
    _open(logged_in_app)

    # p2p-share is row 2 and bounds `operations` per day at tier 1, so its
    # first cell is a bare count — the shape whose headroom sentence is
    # assertable without knowing the byte scale.
    cell = logged_in_app.settings.feature_limit_quota(row=2, cell=0)

    assert cell["label"] == S.features.quota_label(
        dimension=S.features.dimension_operations, window=S.features.window_day
    ), f"the cell must name what it counts over which window: {cell}"

    # "what limit" + "how much is left", from the nest's own numbers. Fresh
    # account: nothing spent, so remaining == limit, and both are the tier-1
    # constant rather than anything this test restates.
    assert " left of " in cell["value"], cell
    remaining, limit = cell["value"].split(" left of ")
    assert remaining == limit, f"nothing is spent yet, so headroom is full: {cell}"
    assert limit.isdigit() and int(limit) > 0, f"a real bound, not a placeholder: {cell}"

    # "which tier set it" — tier 1 is nobody's choice, and it says so.
    assert cell["tier"] == S.features.tier_structural, cell


@pytest.mark.feature("status-quotas-and-limits")
def test_a_byte_volume_cell_is_readable_rather_than_a_raw_count(logged_in_app):
    """A volume bound reaches the screen in its own unit.

    p2p-share's tier-1 volume is 10^12 bytes per day. Rendered raw that is a
    13-digit number, which tells a person nothing — the magnitude is resolved a
    level down (the `BackupLastUploadDisplay` two-level shape) so it lands on
    the shared 1024-unit scale. This is the assertion that fails if any app
    substitutes the raw count into the outer sentence.
    """
    _open(logged_in_app)

    cells = [
        logged_in_app.settings.feature_limit_quota(row=2, cell=i) for i in range(6)
    ]
    volume = [
        c
        for c in cells
        if c["label"].startswith(S.features.dimension_volume)
    ]
    assert volume, f"p2p-share bounds volume at tier 1; found {[c['label'] for c in cells]}"

    value = volume[0]["value"]
    assert any(unit in value for unit in ("KB", "MB", "GB", "TB")), (
        f"a volume bound must carry its unit, got {value!r}"
    )
    assert "1000000000000" not in value, f"a raw byte count reached the screen: {value!r}"


@pytest.mark.feature("status-quotas-and-limits")
def test_no_raw_i18n_key_reaches_the_feature_limits_screen(logged_in_app):
    """The cross-cutting one: several strings here substitute *keys*.

    A cell label composes two of them and the why-line composes one, so a
    client that used plain substitution would paint
    "Uses per features.window_day" at the user. Nothing but an assertion over
    the painted text catches it — it compiles, it renders, and it looks like a
    string until you read it.
    """
    _open(logged_in_app)

    painted: list[str] = []
    for i in range(3):
        row = logged_in_app.settings.feature_limit_row(i)
        painted.extend(row.values())
        for cell in range(6):
            painted.extend(logged_in_app.settings.feature_limit_quota(i, cell).values())

    leaked = [text for text in painted if "features." in text]
    assert not leaked, f"raw i18n keys painted on the feature-limits screen: {leaked}"


def _set_admin_policy(nest_instance, feature: str, policy: dict | None) -> None:
    """Author (or clear) the admin tier's document for one feature.

    An API call rather than a UI one, and legitimately so: the behaviour under
    test in the transparency tests below is the *app's rendering* on all 7
    apps, and this only arranges its precondition (`testing.md` point 8's
    fixture-setup carve-out). The admin AUTHORING journey — the same write made
    through the admin Nest page's editor — is its own test further down
    (`test_an_admin_authors_a_limit_on_the_nest_page_and_it_binds_with_attribution`),
    on the apps that have built the editor. Also the authoring tests' teardown
    safety net: a failed journey must not leave a limit behind on the
    session-scoped nest.
    """
    admin = nest_instance["admin"]
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        payload: dict = {"feature": feature}
        if policy is not None:
            payload["policy"] = policy
        client.call("fauna.features.policy.update", payload)


@pytest.mark.feature("status-quotas-and-limits")
def test_an_admin_limit_reaches_the_screen_naming_the_admin(
    logged_in_app, nest_instance
):
    """**The assertion this whole surface exists for.**

    An admin tightens `p2p-share` to 5 operations/day. The app must then show
    that bound *and say the admin set it* — boundary 4's "which feature, what
    limit, which tier set it", end to end through the real nest.

    Tier 1 alone cannot prove this: every structural bound attributes to
    "Fauna's built-in limits", so a screen that hard-coded that string, or that
    read the tier from the registry instead of from the reply, passes the other
    tests here and fails this one. It also exercises the meet: 5/day is
    *tighter* than tier 1's 500/day, so the admin document must win the MIN.

    Clears the document afterwards — the nest is session-scoped, and a lingering
    admin limit would silently change what every later test in this module sees
    (`e2e-conventions.md`'s standing warning about order-dependent fixtures).
    """
    _require_surface(logged_in_app)
    admin_limit = 5

    _set_admin_policy(
        nest_instance,
        "p2p-share",
        {
            "availability": "limit",
            "operations": {"per_day": admin_limit},
        },
    )
    try:
        # Re-enter Settings: the nav edge re-fetches `fauna.features.status` — a real network round trip, so `feature-limits-section`
        # being visible (it already was, from before this nav) does not by
        # itself mean the NEW read has landed. Poll the value, not the
        # section, for the reason above — no settle-sleep.
        logged_in_app.driver.navigate_to("feed")
        _open(logged_in_app)

        expected_value = S.features.quota_value(
            remaining=str(admin_limit), limit=str(admin_limit)
        )
        cell = logged_in_app.settings.wait_for_feature_limit_quota_value(
            row=2, cell=0, expected_value=expected_value
        )
        assert cell["label"] == S.features.quota_label(
            dimension=S.features.dimension_operations,
            window=S.features.window_day,
        ), f"expected p2p-share's operations/day cell first, got {cell}"

        assert cell["value"] == expected_value, (
            f"the admin's bound must be the one shown, not tier 1's: {cell}"
        )

        assert cell["tier"] == S.features.tier_admin, (
            "the screen must name the ADMIN as the source of this bound — "
            f"got {cell['tier']!r}, which is what a hard-coded or "
            f"registry-derived tier would produce: {cell}"
        )
    finally:
        _set_admin_policy(nest_instance, "p2p-share", None)


@pytest.mark.feature("status-quotas-and-limits")
def test_a_feature_the_admin_turned_off_shows_restricted_with_its_reason(
    logged_in_app, nest_instance
):
    """Boundary 4's other half: a feature you may not use is shown DISABLED,
    with the reason and who set it — never hidden (`dynamic-features.md`
    § What this is NOT).

    The admin denies `p2p-share` outright (`availability: deny`, so the reason
    is `denied_by_admin`; an exhausted zero bound would read `exhausted_admin`
    instead — the deny arm is used because its reason names the tier by
    itself). The row must stay on the page, flip to "Restricted", and paint the
    why-line. `test_feature_limits_section_names_the_gated_members` asserts the
    why-line's ABSENCE on a fresh account; this is its presence, which is the
    half a screen that silently dropped restricted rows would fail.
    """
    _require_surface(logged_in_app)
    _set_admin_policy(nest_instance, "p2p-share", {"availability": "deny"})
    try:
        # Re-enter so the nav edge re-fetches `fauna.features.status`, then poll
        # the ROW (the section marker is already visible from any earlier read).
        logged_in_app.driver.navigate_to("feed")
        _open(logged_in_app)

        row = wait_until(
            lambda: (
                r
                if (r := logged_in_app.settings.feature_limit_row(2)).get("restriction")
                else None
            ),
            15.0,
            diagnose=lambda: (
                f"p2p-share never painted a restriction: "
                f"{logged_in_app.settings.feature_limit_row(2)} "
                f"error={logged_in_app.error_text()!r}"
            ),
        )
        assert row["name"] == S.features.name_p2p_share, (
            f"a denied feature must stay listed, in its place: {row}"
        )
        assert row["status"] == S.features.status_restricted, row
        assert row["restriction"] == S.features.denied_by_admin, (
            f"the why-line must give the reason AND name the admin: {row}"
        )
    finally:
        _set_admin_policy(nest_instance, "p2p-share", None)


# ── Authoring — the admin editor and the self-limits control ────────────────
#
# `dynamic-features.md` § Authoring surfaces: ONE shared editor
# (`feature-policy-editor-*`) opened from two hosts — the admin Nest page
# (tier 3, everyone on this nest) and a row of the Feature limits section above
# (tier 5, only you). These are the journeys: every mutation goes through the
# app's UI (`e2e-conventions.md` point 8); the API is touched only to guarantee
# a clean slate around them, so a red run cannot leak a limit into its siblings
# on the session-scoped nest.

# p2p-share is the last registry member, so row 2 on both hosts (every member is
# carried on a default nest — `test_feature_limits_section_names_the_gated_members`),
# and its first declared cell is operations per day: a bare count, assertable
# without the byte scale.
P2P_ROW = 2
OPS_DAY_CELL = 0


def _require_authoring(app) -> None:
    """tui is the lead app for the authoring half (2026-09-27); the other six
    follow in their batched trickle-down."""
    _require_surface(app)
    if not app.driver.is_tui():
        skip_unbuilt(
            app.driver,
            surface="feature-policy-editor",
            detail="the feature-limits authoring editor (the admin Nest page host "
            "and the self-limits control); its logic is the shared "
            "fauna_client_features::PolicyEditor behind UniFFI + wasm faces, so "
            "the per-app work is painting and forwarding keystrokes",
            tracked="",
        )


def _set_self_limit(nest_instance, user, feature: str, policy: dict | None) -> None:
    """Author (or clear) `user`'s OWN document — the clean-slate guard only."""
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(user["signing_key"].verify_key),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        payload: dict = {"feature": feature}
        if policy is not None:
            payload["policy"] = policy
        client.call("fauna.features.self_limits.update", payload)


def _open_admin_feature_limits(admin_app) -> None:
    _require_authoring(admin_app)
    admin_app.admin.navigate_nest()
    try:
        admin_app.admin.wait_for_feature_limits()
    except TimeoutError:
        raise AssertionError(
            "admin-nest-feature-limits-section never rendered — "
            "fauna.features.policy.get may not have resolved: "
            f"{admin_app.driver.diagnose('admin-nest-feature-limits-section')} "
            f"error={admin_app.error_text()!r}"
        ) from None


def _prefix(rendered_with_marker: str) -> str:
    """The fixed text before a template's substitution (rendered with a NUL
    marker in its place)."""
    return rendered_with_marker.split("\x00", 1)[0]


@pytest.mark.feature("status-quotas-and-limits")
def test_an_admin_authors_a_limit_on_the_nest_page_and_it_binds_with_attribution(
    admin_app, nest_instance
):
    """The admin host, end to end: the admin sets p2p-share to 5 uses a day on
    the Nest page, the row says so in words, and the bound reaches the
    transparency read attributed to the ADMIN — the admin document is nest-wide,
    so it binds the admin's own account too. Then *Remove limit* returns the
    tier to no opinion.

    The editor opens seeded from the AUTHORED document and a save is a whole-
    document replace (§ Authoring surfaces), so the row reads "No limit set"
    before and after, and "On, 1 limit" in between — never the effective meet.
    """
    app = admin_app
    editor = app.feature_policy_editor
    _set_admin_policy(nest_instance, "p2p-share", None)
    try:
        _open_admin_feature_limits(app)
        row = app.admin.wait_for_feature_limit_summary(
            P2P_ROW, S.features.authored_none
        )
        assert row["name"] == S.features.name_p2p_share, row
        assert row["summary"] == S.features.authored_none, (
            f"a tier with no document reads 'No limit set': {row}"
        )

        app.admin.open_feature_limit_editor(P2P_ROW)
        editor.wait_open()
        assert editor.title() == S.features.editor_title_admin(
            feature=S.features.name_p2p_share
        ), editor.title()
        assert editor.is_on(), "a feature with no document opens On"
        assert not editor.remove_present(), (
            "Remove limit renders only while a document exists"
        )
        assert editor.cell(OPS_DAY_CELL)["label"] == S.features.quota_label(
            dimension=S.features.dimension_operations, window=S.features.window_day
        ), editor.cell(OPS_DAY_CELL)

        editor.set_cell(OPS_DAY_CELL, "5")
        editor.save()
        assert editor.wait_for_status(S.features.editor_saved) == S.features.editor_saved, (
            f"the save never reported success: status={editor.status()!r} "
            f"error={app.error_text()!r}"
        )
        # Re-read after the save: the editor re-seeds from the authored document.
        assert editor.cell(OPS_DAY_CELL)["input"] == "5", editor.cell(OPS_DAY_CELL)
        assert editor.remove_present(), "a document now exists, so Remove renders"
        row = app.admin.wait_for_feature_limit_summary(
            P2P_ROW, S.features.authored_limited_one
        )
        assert row["summary"] == S.features.authored_limited_one, row
        editor.cancel()

        # It binds — on the transparency read, naming the admin.
        app.settings._navigate_subpage("status")
        app.settings.wait_for_feature_limits()
        expected = S.features.quota_value(remaining="5", limit="5")
        cell = app.settings.wait_for_feature_limit_quota_value(
            row=P2P_ROW, cell=OPS_DAY_CELL, expected_value=expected
        )
        assert cell["value"] == expected, cell
        assert cell["tier"] == S.features.tier_admin, (
            f"the bound the admin authored must be attributed to the admin: {cell}"
        )

        # Remove limit — the absent policy, back to no opinion.
        _open_admin_feature_limits(app)
        app.admin.open_feature_limit_editor(P2P_ROW)
        editor.wait_open()
        editor.remove()
        assert editor.wait_for_status(S.features.editor_removed) == S.features.editor_removed, (
            f"status={editor.status()!r} error={app.error_text()!r}"
        )
        row = app.admin.wait_for_feature_limit_summary(P2P_ROW, S.features.authored_none)
        assert row["summary"] == S.features.authored_none, row
    finally:
        editor.close_if_open()
        _set_admin_policy(nest_instance, "p2p-share", None)


@pytest.mark.feature("status-quotas-and-limits")
def test_a_looser_value_is_never_refused_but_says_it_has_no_effect(
    admin_app, nest_instance
):
    """Tighten-only is RENDERED, never enforced twice (§ Authoring surfaces):
    typing a value looser than the ceiling (tier 1's built-in limit, for the
    admin tier) paints the no-effect note naming who already binds — and a
    tighter value paints none. The pair is the assertion: a note that is always
    on, or never on, fails one half.
    """
    app = admin_app
    editor = app.feature_policy_editor
    _set_admin_policy(nest_instance, "p2p-share", None)
    try:
        _open_admin_feature_limits(app)
        app.admin.open_feature_limit_editor(P2P_ROW)
        editor.wait_open()

        editor.set_cell(OPS_DAY_CELL, "100000000")
        cell = editor.wait_for_cell_note(OPS_DAY_CELL, present=True)
        prefix = _prefix(S.features.no_effect_structural(limit="\x00"))
        assert cell.get("note", "").startswith(prefix), (
            "a value looser than the built-in limit owes the no-effect note "
            f"naming Fauna's built-in limit: {cell}"
        )
        shown = cell["note"][len(prefix):].rstrip(".")
        assert shown.isdigit() and int(shown) < 100_000_000, (
            f"the note must name the ceiling that binds instead: {cell}"
        )

        editor.set_cell(OPS_DAY_CELL, "1")
        cell = editor.wait_for_cell_note(OPS_DAY_CELL, present=False)
        assert "note" not in cell, f"a tighter value has an effect — no note: {cell}"
        editor.cancel()
    finally:
        editor.close_if_open()
        _set_admin_policy(nest_instance, "p2p-share", None)


@pytest.mark.feature("status-quotas-and-limits")
def test_you_set_your_own_limit_and_the_row_above_binds_without_renavigating(
    logged_in_app, nest_instance, test_user
):
    """The self host: a person limits themselves from the Feature limits row,
    and the SAME row's transparency cell shows the new bound, attributed to
    "Your own setting", with no re-navigation (the self host re-reads the
    transparency rows after every save). Then Off — self-exclusion is the case
    the tier exists for — shows the row Restricted with the self reason, and
    Remove limit returns it to no opinion.
    """
    app = logged_in_app
    editor = app.feature_policy_editor
    _set_self_limit(nest_instance, test_user, "p2p-share", None)
    try:
        _require_authoring(app)
        _open(app)
        row = app.settings.wait_for_feature_limit_row(
            P2P_ROW, lambda r: r.get("own_summary") == S.features.authored_none
        )
        assert row.get("own_summary") == S.features.authored_none, row

        app.settings.open_own_feature_limit_editor(P2P_ROW)
        editor.wait_open()
        assert editor.title() == S.features.editor_title_self(
            feature=S.features.name_p2p_share
        ), editor.title()

        editor.set_cell(OPS_DAY_CELL, "3")
        editor.save()
        assert editor.wait_for_status(S.features.editor_saved) == S.features.editor_saved, (
            f"status={editor.status()!r} error={app.error_text()!r}"
        )
        expected = S.features.quota_value(remaining="3", limit="3")
        cell = app.settings.wait_for_feature_limit_quota_value(
            row=P2P_ROW, cell=OPS_DAY_CELL, expected_value=expected
        )
        assert cell["value"] == expected, (
            f"the row above must show the new binding without a re-navigation: {cell}"
        )
        assert cell["tier"] == S.features.tier_self, cell
        row = app.settings.wait_for_feature_limit_row(
            P2P_ROW, lambda r: r.get("own_summary") == S.features.authored_limited_one
        )
        assert row.get("own_summary") == S.features.authored_limited_one, row

        # Off — a person may turn a gated feature off for themselves.
        editor.choose_off()
        editor.save()
        row = app.settings.wait_for_feature_limit_row(
            P2P_ROW, lambda r: r.get("restriction") == S.features.denied_by_self
        )
        assert row["status"] == S.features.status_restricted, row
        assert row.get("restriction") == S.features.denied_by_self, row
        assert row.get("own_summary") == S.features.authored_off, row

        # Remove limit — back to no opinion; the row is Available again.
        editor.remove()
        row = app.settings.wait_for_feature_limit_row(
            P2P_ROW, lambda r: r.get("own_summary") == S.features.authored_none
        )
        assert row.get("own_summary") == S.features.authored_none, row
        assert row["status"] == S.features.status_available, row
        assert "restriction" not in row, row
    finally:
        editor.close_if_open()
        _set_self_limit(nest_instance, test_user, "p2p-share", None)


@pytest.mark.feature("status-quotas-and-limits")
def test_the_editor_writes_disable_offline_and_are_never_queued(
    logged_in_app, nest_instance, test_user
):
    """Both writes are `OnlineOnly` (§ Authoring surfaces): with no nest the
    Save and Remove buttons desensitize while Cancel — pure local UI — stays
    live beside them; the nest coming back re-enables them. The pairing is the
    assertion (`test_offline_gate.py`'s rule): a gate that greyed everything
    would pass a one-sided check.
    """
    from common.nest import start_nest_in_place, stop_nest

    app = logged_in_app
    editor = app.feature_policy_editor
    _set_self_limit(
        nest_instance,
        test_user,
        "p2p-share",
        {"availability": "limit", "operations": {"per_day": 50}},
    )
    try:
        _require_authoring(app)
        _open(app)
        app.settings.wait_for_feature_limit_row(
            P2P_ROW, lambda r: r.get("own_summary") == S.features.authored_limited_one
        )
        app.settings.open_own_feature_limit_editor(P2P_ROW)
        editor.wait_open()
        assert app.driver.is_enabled("feature-policy-editor-save-button")
        assert editor.remove_present(), "a document exists, so Remove renders"
        assert app.driver.is_enabled("feature-policy-editor-remove-button")

        stop_nest(nest_instance, graceful=True)
        try:
            wait_until(
                lambda: (
                    not app.driver.is_enabled("feature-policy-editor-save-button")
                    and not app.driver.is_enabled("feature-policy-editor-remove-button")
                ),
                90.0,
                diagnose=lambda: f"Save / Remove stayed enabled with no nest: "
                f"error={app.error_text()!r}",
            )
            assert app.driver.is_enabled("feature-policy-editor-cancel-button"), (
                "Cancel is local UI and must stay live offline — a gate that "
                "greys everything is over-claiming"
            )
        finally:
            start_nest_in_place(nest_instance)
        wait_until(
            lambda: app.driver.is_enabled("feature-policy-editor-save-button"),
            120.0,
            diagnose=lambda: f"Save never re-enabled: error={app.error_text()!r}",
        )
        editor.cancel()
    finally:
        editor.close_if_open()
        _set_self_limit(nest_instance, test_user, "p2p-share", None)


# ── Authoring — the guardian host (family-safety.md § App surface) ──────────
#
# The third host of the same editor: a guardian limits a feature for ONE ward
# from that ward's policy screen. The limit rides `fauna.family.policy.update`
# as the policy document's `features` sub-document, written only by this editor.


def _open_ward_feature_limits(app, request, nest_instance, guardian_identity, ward_handle):
    """Sign the app in as the guardian and land on `ward_handle`'s policy
    editor with the feature-limits rows painted."""
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    wait_until(
        lambda: ward_handle in app.family.ward_handles(),
        15.0,
        diagnose=lambda: f"expected ward {ward_handle!r} among "
        f"{app.family.ward_handles()!r}; error={app.error_text()!r}",
    )
    app.family.select_ward_by_handle(ward_handle)
    try:
        app.family.wait_for_feature_limits()
    except TimeoutError:
        raise AssertionError(
            "family-policy-feature-limits rows never rendered — "
            "fauna.family.status or fauna.nest.info may not have resolved: "
            f"{app.driver.diagnose('family-policy-feature-limits-section')} "
            f"error={app.error_text()!r}"
        ) from None


@pytest.mark.feature("family-safety")
def test_a_guardian_limits_a_feature_for_one_child_and_the_child_sees_who_set_it(
    admin_app, request, nest_instance, family_pair
):
    """The guardian host, end to end: from the ward's policy screen the guardian
    sets p2p-share to 3 uses a day, the row says so in words, and the WARD's own
    Feature limits row shows that bound attributed to their guardian. Back as
    the guardian the editor reads the document back, and *Remove limit* returns
    the tier to no opinion.

    On the way: a value looser than what an outer tier already allows is never
    refused but says it has no effect, naming who binds. The admin's 5 a day is
    arranged through the API (fixture setup — `testing.md` point 8's carve-out;
    the admin's own authoring journey is the test above), because the ceiling
    the guardian's editor notes against is the meet of the tiers OUTSIDE the
    guardian's, composed by the nest for the ward.
    """
    from conftest import _login_app_as

    app = admin_app
    app.family.require_feature_limits_supported()
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair
    editor = app.feature_policy_editor
    admin_bound = 5
    _set_admin_policy(
        nest_instance,
        "p2p-share",
        {"availability": "limit", "operations": {"per_day": admin_bound}},
    )
    try:
        _open_ward_feature_limits(app, request, nest_instance, guardian_identity, ward_handle)
        row = app.family.wait_for_feature_limit_summary(P2P_ROW, S.features.authored_none)
        assert row["name"] == S.features.name_p2p_share, row
        assert row["summary"] == S.features.authored_none, (
            f"a guardian who has set nothing reads 'No limit set': {row}"
        )

        app.family.open_feature_limit_editor(P2P_ROW)
        editor.wait_open()
        assert editor.title() == S.features.editor_title_guardian(
            feature=S.features.name_p2p_share, ward=ward_handle
        ), editor.title()
        assert not editor.remove_present(), (
            "Remove limit renders only while a document exists"
        )

        # Looser than the admin's bound: noted, never refused.
        editor.set_cell(OPS_DAY_CELL, "9")
        cell = editor.wait_for_cell_note(OPS_DAY_CELL, present=True)
        assert cell.get("note") == S.features.no_effect_admin(limit=str(admin_bound)), (
            "a value looser than the admin's bound owes the no-effect note "
            f"naming the admin: {cell}"
        )
        editor.set_cell(OPS_DAY_CELL, "3")
        cell = editor.wait_for_cell_note(OPS_DAY_CELL, present=False)
        assert "note" not in cell, f"a tighter value has an effect — no note: {cell}"

        editor.save()
        assert editor.wait_for_status(S.features.editor_saved) == S.features.editor_saved, (
            f"the save never reported success: status={editor.status()!r} "
            f"error={app.error_text()!r}"
        )
        row = app.family.wait_for_feature_limit_summary(
            P2P_ROW, S.features.authored_limited_one
        )
        assert row["summary"] == S.features.authored_limited_one, row
        editor.cancel()

        # The ward's side: their own row shows the limit binding, the guardian named.
        app.driver.reset()
        _login_app_as(app, request, nest_instance, ward_identity)
        _open(app)
        expected = S.features.quota_value(remaining="3", limit="3")
        cell = app.settings.wait_for_feature_limit_quota_value(
            row=P2P_ROW, cell=OPS_DAY_CELL, expected_value=expected
        )
        assert cell["value"] == expected, (
            f"the guardian's bound must be the one the ward sees: {cell}"
        )
        assert cell["tier"] == S.features.tier_guardian, (
            f"the ward's row must say their GUARDIAN set this limit: {cell}"
        )

        # Back as the guardian, in a fresh session: the editor reads the stored
        # document back, and Remove limit clears it.
        _open_ward_feature_limits(app, request, nest_instance, guardian_identity, ward_handle)
        row = app.family.wait_for_feature_limit_summary(
            P2P_ROW, S.features.authored_limited_one
        )
        assert row["summary"] == S.features.authored_limited_one, row
        app.family.open_feature_limit_editor(P2P_ROW)
        editor.wait_open()
        assert editor.cell(OPS_DAY_CELL)["input"] == "3", editor.cell(OPS_DAY_CELL)
        assert editor.remove_present(), "a document now exists, so Remove renders"
        editor.remove()
        assert editor.wait_for_status(S.features.editor_removed) == S.features.editor_removed, (
            f"status={editor.status()!r} error={app.error_text()!r}"
        )
        row = app.family.wait_for_feature_limit_summary(P2P_ROW, S.features.authored_none)
        assert row["summary"] == S.features.authored_none, row
    finally:
        editor.close_if_open()
        _set_admin_policy(nest_instance, "p2p-share", None)
