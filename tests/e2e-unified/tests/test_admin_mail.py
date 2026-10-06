"""admin-mail page (admin.md § 6 Mail — the flat mail-policy form).

linux is the **lead** client for the mail-UX seed (per the mail-UX-gaps plan §
the 12 per-app seed tracks — the linux row); the shared
`fauna_client_mail_settings::admin_policy::MailPolicyMachine` + the linux GTK page
are the prior art the other five apps lift over the `build_mail_policy_machine`
UniFFI/wasm export. They deselect via --client until they implement the page.

tier_3: a real client driver (linux GTK) against a real `fauna-nest` binary. The
round-trip test drives the page (set a knob via the in-process agent → Save →
shared machine PUT → nest → re-hydrate via `get_mail_config` → re-render) and
asserts ground truth over the same Admin WS-RPC `fauna.bridges.get_mail_config`
twin the page hydrates from — not UI introspection alone.
"""
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import _provision_baseline_contributor, _withdraw_baseline_contributors
from helpers.app_surface import app_name, skip_unbuilt
from tests.api import ws_api

# Module-level: linux is the lead app and renders ALL FIVE projected policy
# groups (spam/auth/submission/imap/outbound) plus the dual-read alias group.
# windows has lifted the FULL page (Bundle B, a windows mail-UX follow-up — the
# AdminMailViewModel over the `build_mail_policy_machine` UniFFI export + the
# AdminMailPage on the AdminShell), so it carries the module-level `windows` mark
# alongside linux: every test in this file is a windows-parity test (the solo
# FlaUI run is deferred — FlaUI flakes on
# win-arm64; the deterministic VM unit tests + name/xbind lints gate windows here).
# web lifted ALL SIX groups 2026-06-03 (a web admin-mail-groups follow-up — the
# +page.svelte renders the submission/imap/outbound + dual-read alias sections from
# the same shared wasm `MailPolicyMachine` twin). Every test in this file now carries
# an explicit `@pytest.mark.web` (platform marks are additive + OR'd by conftest's
# --client deselection), so the full page + the IMAP/alias round-trips run on web too.
# macos + ios lifted the FULL page 2026-06-12 (a macos mail-admin follow-up, Bundle B —
# shared FaunaKit `AdminMailView`/`AdminMailVM` over the same UniFFI `MailPolicyMachine`
# twin, all seven groups + the conn-cap widget). They're added at module level (the
# whole page is built on both). ✅ macОS CONFIRMED GREEN — the former AutomationMode
# wedge is retired (the in-process bare-binary driver replaced XCUITest, no reboot);
# every test here passes `--client macos` (last full run 2026-07-02, `just mac-debug`
# then `test_admin_mail.py --client macos` 10/10). iOS
# uses the identical shared `AdminMailView`, so the `ios` markers are accurate by
# construction; a `--client ios` render-confirm is banked for the next warm iOS build.
# tui lifted the FULL page 2026-07-18 (M8 admin-mail): the `admin/mail.rs` sub-page
# is a dumb renderer of the same shared `MailPolicyMachine`, painting all seven
# groups (spam / auth / submission / imap / outbound / alias + the two deployment
# toggles) and the publish-baseline control. It runs `--client tui` on every test
# here (the whole page is built), so the module carries `tui` at module level too.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]


def _admin_client(nest_instance):
    """An Admin WS-RPC client keyed on the nest's admin identity (same shape as
    test_mail_admin_policy) — the ground-truth read path for the round-trip."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _nest_now(nest_instance, read):
    """What the nest holds for this field RIGHT NOW — the value the page must
    hydrate to, which is not the catalog default.

    `nest_instance` is `scope="session"` and shared by every module and every app
    parametrization (`conftest.py`), so the catalog default is never a safe
    expectation: any module that ran earlier may have saved a policy. Before
    `_leave_the_session_nest_as_found` existed, this file's own saves did exactly
    that to its second and third apps — a `== <catalog default>` hydrate assert
    failed before a single key was typed, which produced a
    table (12 failures on linux+tui, 0 on web, purely because web ran first in
    the sweep order) that was read as a native-vs-web product split, then as a
    bug in the `http_bridge` driver's `clear_and_type`, neither of which was
    involved.

    Asserting against this read is also the STRONGER claim. "The page shows 5"
    only pins hydration when the nest happens to hold 5; "the page shows what
    `get_mail_config` returns" pins that hydration tracks the nest at all times,
    which is what these tests exist to prove. The catalog defaults themselves are
    a nest-side fact and belong to the API tier, not to a UI round-trip.
    """
    with _admin_client(nest_instance) as client:
        return read(client)


def _distinct_target(before, preferred, alternate):
    """A save target guaranteed to differ from what the nest already holds, so
    the round-trip proves a real write rather than coinciding with the value
    that was already there (which a fixed literal cannot guarantee once a
    sibling app has run the same test against this shared nest)."""
    return alternate if before == preferred else preferred


def _wait(predicate, timeout: float = 15.0, interval: float = 0.3) -> bool:
    """Poll `predicate` until true or the deadline (the form hydrates + saves
    asynchronously: admin nav → MailPolicyMachine → WS-RPC → render)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


#: The five policy groups `get_mail_config` projects, each with the kind that
#: writes it. With the alias policy (its own read twin) and the auto-enable
#: toggle (read via `fauna.setup.status`), this is every deployment-wide knob a
#: test in this file saves.
_PROJECTED_POLICY_GROUPS = {
    "spam": "fauna.bridges.put_spam_policy",
    "auth": "fauna.bridges.put_auth_policy",
    "submission": "fauna.bridges.put_submission_policy",
    "imap": "fauna.bridges.put_imap_policy",
    "outbound": "fauna.bridges.put_outbound_policy",
}


def _policy_snapshot(client):
    config = client.call("fauna.bridges.get_mail_config", {})
    return {
        **{group: config[group] for group in _PROJECTED_POLICY_GROUPS},
        "alias": client.call("fauna.bridges.get_alias_policy", {}),
        "auto_enable": client.call("fauna.setup.status", {})[
            "auto_enable_mail_for_new_users"
        ],
    }


@pytest.fixture(autouse=True)
def _leave_the_session_nest_as_found(nest_instance):
    """Put back every deployment-wide mail policy a test here saved.

    Every save test in this file persists a new value into the session nest's
    one mail config — that is what a round-trip is — and nothing used to put it
    back, so every LATER module on the same nest ran under it. Measured in the
    2026-09-14 whole-suite ``--app linux`` sweep: the alias round-trip left the
    per-account exact-alias cap at 3, and ``test_mail_aliases_import.py``, whose
    actor holds three aliases by the time it imports, got every fresh line back
    ``alias cap reached``. The spam thresholds, the
    Bayesian weight, the IMAP IDLE timeout, the per-IP connection cap and the
    auto-enable-mail-for-new-users toggle (flipped OFF) were left moved the same
    way, under every mail module that sorts after this one.

    A group is PUT back verbatim from its own read only when the test changed
    it, and the whole snapshot is read again afterwards and compared, so a
    restore that did not take fails here instead of leaking silently again.
    """
    with _admin_client(nest_instance) as client:
        before = _policy_snapshot(client)
    yield
    with _admin_client(nest_instance) as client:
        now = _policy_snapshot(client)
        for group, kind in _PROJECTED_POLICY_GROUPS.items():
            if now[group] != before[group]:
                client.call(kind, before[group])
        if now["alias"] != before["alias"]:
            client.call("fauna.bridges.put_alias_policy", before["alias"])
        if now["auto_enable"] != before["auto_enable"]:
            client.call(
                "fauna.bridges.set_auto_enable_mail_for_new_users",
                {"enabled": before["auto_enable"]},
            )
        restored = _policy_snapshot(client)
    assert restored == before, (
        "a test left the session nest's mail policy changed and the restore did "
        f"not take; before={before!r} after-restore={restored!r}"
    )


@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_mail_policy_page_renders(admin_app):
    """The flat admin-mail policy form renders its core controls across the
    mail-enable + spam/inbound-perimeter + auth-enforcement groups. The
    submission/imap/outbound groups are asserted by `test_new_policy_groups_render`
    (now also web, as of the 2026-06-03 web lift)."""
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"admin-mail-enabled-toggle missing. error: {admin_app.error_text()!r}"
    )
    for element_id in (
        "admin-mail-enabled-toggle",
        "admin-mail-auto-enable-new-users-toggle",
        "admin-mail-spam-threshold-junk",
        "admin-mail-dnsbl-servers",
        "admin-mail-fcrdns-mode-select",
        "admin-mail-spam-save-button",
        "admin-mail-auth-enforce-dmarc-toggle",
        "admin-mail-auth-max-failures-input",
        "admin-mail-auth-save-button",
    ):
        assert admin_app.admin.mail_field_present(element_id), (
            f"{element_id} missing. error: {admin_app.error_text()!r}"
        )


@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_new_policy_groups_render(admin_app):
    """Renders the three follow-on projected groups — Submission / IMAP /
    Outbound — beyond the spam/auth core. Web lifted these 2026-06-03
    (the web admin-mail-groups follow-up), rendering the same shared wasm
    `MailPolicyMachine` twin linux leads, so this now runs on web too."""
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )
    for element_id in (
        "admin-mail-submission-max-per-day-input",
        "admin-mail-submission-save-button",
        "admin-mail-imap-idle-timeout-input",
        "admin-mail-imap-delete-nonempty-select",
        "admin-mail-imap-save-button",
        "admin-mail-outbound-retry-schedule",
        "admin-mail-outbound-ipv6-toggle",
        "admin-mail-outbound-treat-5xx-transient",
        "admin-mail-outbound-save-button",
    ):
        assert _wait(lambda eid=element_id: admin_app.admin.mail_field_present(eid)), (
            f"{element_id} missing. error: {admin_app.error_text()!r}"
        )


@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_spam_threshold_save_round_trips(admin_app, nest_instance):
    """Raising the Junk spam threshold via the UI writes through the shared
    MailPolicyMachine → `fauna.bridges.put_spam_policy` → nest, proven by reading
    the Admin `get_mail_config` twin over the wire (the same read the page
    hydrates from). Catalog default 5; we set 7 (6 if the nest already holds 7 —
    see `_nest_now`: this config is shared with every other app's run)."""
    before = _nest_now(
        nest_instance,
        lambda c: c.call("fauna.bridges.get_mail_config", {})["spam"][
            "max_score_before_spam_folder"
        ],
    )
    target = _distinct_target(before, 7, 6)

    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )

    junk = "admin-mail-spam-threshold-junk"
    # The form hydrates the junk entry from get_mail_config — to whatever the nest
    # holds, which is the catalog default only on the first app to run this test.
    assert _wait(lambda: admin_app.admin.mail_field_text(junk).strip() == str(before)), (
        f"junk threshold did not hydrate to the nest's current value {before}; "
        f"got {admin_app.admin.mail_field_text(junk)!r}, error: {admin_app.error_text()!r}"
    )

    admin_app.admin.set_mail_field(junk, str(target))
    admin_app.admin.save_mail_spam()

    client = _admin_client(nest_instance)
    with client:
        # Ground truth: the Admin read twin reflects the persisted override.
        assert _wait(
            lambda: client.call("fauna.bridges.get_mail_config", {})["spam"][
                "max_score_before_spam_folder"
            ]
            == target
        ), (
            f"put_spam_policy did not persist max_score_before_spam_folder={target} "
            f"(get_mail_config still shows the old value). error: {admin_app.error_text()!r}"
        )

    # And the page re-rendered the persisted value (render() overwrites the entry
    # from the get_mail_config snapshot — proving it round-tripped, not just kept
    # the typed text).
    assert _wait(lambda: admin_app.admin.mail_field_text(junk).strip() == str(target)), (
        f"the page did not re-render the persisted junk threshold ({target}). "
        f"error: {admin_app.error_text()!r}"
    )
    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid save: {admin_app.error_text()!r}"
    )


@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_spam_tier2_training_knobs_render(admin_app):
    """The four Tier-2 per-user spam-training knobs render in the admin-mail Spam
    group and hydrate to their catalog defaults from `get_mail_config`
    (mail-policy-config.md § Spam (per-user training) — bayesian_weight_milli=700,
    bayesian_min_samples=50, bayesian_full_confidence_samples=200,
    training_history_retention_days=30). Re-homed into the flat page on all 6
    apps 2026-07-01 — plain numeric
    inputs on the shared `SpamPolicyView`/`MailPolicyMachine` that full-PUT through
    the same `admin-mail-spam-save-button`. `bayesian_weight_milli` renders as a
    0-1000 milli integer (700 = 0.7)."""
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )
    # The knobs sit at the tail of the Spam group; assert built (count), not
    # on-screen — a control low in a long page can be built-but-not-showing.
    for element_id, default in (
        ("admin-mail-spam-bayesian-weight", "700"),
        ("admin-mail-spam-bayesian-min-samples", "50"),
        ("admin-mail-spam-bayesian-full-confidence-samples", "200"),
        ("admin-mail-spam-training-history-retention", "30"),
    ):
        assert _wait(lambda eid=element_id: admin_app.admin.mail_field_present(eid)), (
            f"{element_id} missing — Tier-2 knob not rendered. "
            f"error: {admin_app.error_text()!r}"
        )
        assert _wait(
            lambda eid=element_id, d=default: admin_app.admin.mail_field_text(eid).strip() == d
        ), (
            f"{element_id} did not hydrate to the catalog default {default}; got "
            f"{admin_app.admin.mail_field_text(element_id)!r}, error: {admin_app.error_text()!r}"
        )


@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_publish_spam_baseline_controls_render(admin_app):
    """The deployment-baseline publish button + result text render in the
    admin-mail Spam group (mail-policy-config.md § Spam — deployment-baseline
    publish; all 6 apps, 2026-07-01). The result is empty until the admin
    clicks Publish, so this asserts presence only; the withheld round-trip is
    `test_publish_spam_baseline_withheld`."""
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )
    for element_id in (
        "admin-mail-publish-spam-baseline-button",
        "admin-mail-publish-spam-baseline-result",
    ):
        assert _wait(lambda eid=element_id: admin_app.admin.mail_field_present(eid)), (
            f"{element_id} missing — publish-baseline control not rendered. "
            f"error: {admin_app.error_text()!r}"
        )


@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_publish_spam_baseline_withheld(admin_app):
    """Clicking Publish deployment baseline on a fresh nest — which has zero opt-in
    contributors — dispatches `fauna.bridges.publish_spam_baseline`, and the
    k-anonymity floor (`BASELINE_MIN_CONTRIBUTORS = 3`, mail-spam.md § Cold start
    Path 2) withholds: the result text renders the "too few contributors" message.
    Proves the client Publish button → shared `MailPolicyMachine` → nest dispatch +
    the aggregate-only reply render. There is no read twin — the reply is stashed
    client-side, so the UI result IS the assertion.

    All 7 apps. The four *live-read* renderers (linux GTK, web `<p>`, windows
    `TextBlock`, android `Text`) reflect the dynamically-updated result immediately.
    apple's shared FaunaKit renders it via `.automationValue(id, text: {...})`, whose
    in-process reader is captured once at `.onAppear`; the publish dispatch does a
    *no-refresh* snapshot update, so the reader kept serving the stale empty value
    until the `.id()`-on-content refresh fix (2026-07-02, `fix(macos,ios): admin-mail
    publish-baseline result render staleness`) — a value change now forces a fresh
    identity → re-`.onAppear` → re-register the current closure. (The four knob fields
    never had this gap: they read a live `.automationField` binding, not a captured
    struct `let`.) macos re-confirmed green 2026-07-02; iOS rides the same shared view
    (banked for the next warm iOS build)."""
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )
    assert _wait(
        lambda: admin_app.admin.mail_field_present("admin-mail-publish-spam-baseline-button")
    ), (
        "admin-mail-publish-spam-baseline-button missing. "
        f"error: {admin_app.error_text()!r}"
    )

    admin_app.admin.publish_spam_baseline()

    # Fresh nest: 0 opt-in contributors < 3 → the floor withholds and the result
    # renders "Not published — too few contributors (0); at least 3 must opt in."
    assert _wait(
        lambda: "too few contributors"
        in admin_app.admin.publish_spam_baseline_result().lower()
    ), (
        "publish-baseline result did not render the k-anonymity withheld message; "
        f"got {admin_app.admin.publish_spam_baseline_result()!r}, "
        f"error: {admin_app.error_text()!r}"
    )
    assert not admin_app.error_text().strip(), (
        f"unexpected error after clicking Publish: {admin_app.error_text()!r}"
    )


#: Apps that render the standing-publish toggle and the baseline state text
#: (tui is the lead app; the other six follow in their batched trickle-down).
_STANDING_BUILT_APPS = {"tui"}


def _opted_in_contributors(nest_instance, seal_helper_binary, tag, n=3):
    """`n` fresh mail users, each opted in to the baseline with a sealed model, a
    copy sealed to the nest's aggregation holder and a keyless grant to it — the
    precondition for a publish to clear the contributor floor, since the holder
    merges the baseline from those copies off-box (the nest can read no model).
    Setup, not the journey: the journey is the admin's."""
    return [
        _provision_baseline_contributor(
            nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
            local_part=f"admin-baseline-{tag}-{i}", token=f"qz{tag}{i}wx", spam_messages=5,
        )[0]
        for i in range(n)
    ]


@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_standing_baseline_toggle_and_state_text(
    admin_app, nest_instance, mail_bridge_mda, seal_helper_binary, test_user,
):
    """The admin keeps a shared spam baseline published, sees its state, and
    turning standing publish off withdraws it (`mail-spam.md` § Cold start Path 2
    → *Standing publish*).

    toggle on (UI) → the nest holds standing on → three contributors opt in →
    publish now (UI) → the state text reads "Published over 3 contributors on
    <date>." → publish now again with nothing new → the delta floor defers: the
    result and the state both say "Waiting for more contributor activity." →
    toggle off (UI) → the nest serves nothing and the state text reads "No
    baseline published." The 24-hour cadence itself is nest-side and covered
    there; "publish now" is the same run.

    `mail_bridge_mda` is the aggregation holder the publish drives: every model
    rests sealed, so only the holder merges a baseline, from the contributors'
    sealed copies. The session `test_user` is opted out first — the GUI spam
    tests leave it an opted-in contributor, which would make "over 3" read
    "over 4" (the same normalisation `test_spam_baseline_drain.py` makes).

    The baseline is a deployment singleton on the shared session nest, so the
    contributors opt back out at the end (which also withdraws), leaving
    `test_publish_spam_baseline_withheld`'s zero-contributor premise intact; the
    autouse fixture restores the spam group, standing publish included."""
    toggle = "admin-mail-spam-baseline-standing-toggle"
    state_id = "admin-mail-spam-baseline-state"
    if app_name(admin_app.driver) not in _STANDING_BUILT_APPS:
        skip_unbuilt(
            admin_app.driver,
            surface=toggle,
            detail="standing baseline publish and its state text are built on tui first",
            tracked="",
        )

    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )
    for element_id in (toggle, state_id):
        assert _wait(lambda: admin_app.admin.mail_field_present(element_id)), (
            f"{element_id} missing. error: {admin_app.error_text()!r}"
        )

    contributors = []
    with _admin_client(nest_instance) as client:
        def standing():
            return client.call("fauna.bridges.get_mail_config", {})["spam"][
                "baseline_standing_publish"]

        def served():
            return client.call("fauna.bridges.get_spam_baseline_state", {})

        try:
            if standing():
                # A sibling left it on: start from off, through the UI too.
                admin_app.admin.toggle_mail_baseline_standing()
                assert _wait(lambda: standing() is False), (
                    f"could not start from standing off. error: {admin_app.error_text()!r}"
                )

            admin_app.admin.toggle_mail_baseline_standing()
            assert _wait(lambda: standing() is True), (
                "the toggle did not persist standing publish ON. "
                f"error: {admin_app.error_text()!r}"
            )
            assert _wait(lambda: served()["standing"] is True)

            ws_api.baseline_contribution_set(nest_instance["port"], test_user, False)
            contributors = _opted_in_contributors(nest_instance, seal_helper_binary, "standing")
            admin_app.admin.publish_spam_baseline()
            assert _wait(lambda: served()["published"] is True), (
                f"publish now over three new contributors did not serve a baseline: "
                f"{served()!r}. error: {admin_app.error_text()!r}"
            )
            assert _wait(
                lambda: admin_app.admin.spam_baseline_state().startswith("Published over 3 ")
            ), (
                f"state text after publish: {admin_app.admin.spam_baseline_state()!r}, "
                f"nest: {served()!r}"
            )
            assert "waiting" not in admin_app.admin.spam_baseline_state().lower()

            # Nothing changed since the served baseline → the delta floor defers.
            admin_app.admin.publish_spam_baseline()
            assert _wait(lambda: served()["deferred"] is True), (
                f"an unchanged republish was not deferred: {served()!r}"
            )
            assert _wait(
                lambda: "waiting for more contributor activity"
                in admin_app.admin.publish_spam_baseline_result().lower()
            ), (
                "a deferred publish must not read as withheld; result: "
                f"{admin_app.admin.publish_spam_baseline_result()!r}"
            )
            assert _wait(
                lambda: admin_app.admin.spam_baseline_state().startswith("Published over 3 ")
                and admin_app.admin.spam_baseline_state().endswith(
                    "Waiting for more contributor activity.")
            ), f"state text after a deferred run: {admin_app.admin.spam_baseline_state()!r}"

            admin_app.admin.toggle_mail_baseline_standing()
            assert _wait(lambda: standing() is False), (
                f"the toggle did not persist standing OFF. error: {admin_app.error_text()!r}"
            )
            assert _wait(lambda: served()["published"] is False), (
                f"turning standing publish off must withdraw the baseline: {served()!r}"
            )
            assert _wait(
                lambda: admin_app.admin.spam_baseline_state().startswith("No baseline published.")
            ), f"state text after off: {admin_app.admin.spam_baseline_state()!r}"
            assert not admin_app.error_text().strip(), (
                f"unexpected error on the standing journey: {admin_app.error_text()!r}"
            )
        finally:
            _withdraw_baseline_contributors(nest_instance, contributors)


@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_bayesian_weight_save_round_trips(admin_app, nest_instance):
    """Raising the Bayesian-weight knob writes through the shared MailPolicyMachine
    → `fauna.bridges.put_spam_policy` → nest (the SAME full-PUT the junk-threshold
    test proves, now carrying the four additive Tier-2 fields), read back over the
    Admin `get_mail_config` twin. The knob renders as a 0-1000 milli integer
    (700 = 0.7); we set 850 (800 if the nest already holds 850 — see `_nest_now`).
    The other bayesian fields keep their hydrated defaults (min=50, full=200), so
    the handler's `full_confidence <= min_samples` guard does not trip."""
    before = _nest_now(
        nest_instance,
        lambda c: c.call("fauna.bridges.get_mail_config", {})["spam"][
            "bayesian_weight_milli"
        ],
    )
    target = _distinct_target(before, 850, 800)

    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )

    weight = "admin-mail-spam-bayesian-weight"
    # Hydrates from get_mail_config — the nest's current milli value, which is the
    # catalog default (700) only on the first app to run this test.
    assert _wait(lambda: admin_app.admin.mail_field_text(weight).strip() == str(before)), (
        f"bayesian weight did not hydrate to the nest's current value {before}; got "
        f"{admin_app.admin.mail_field_text(weight)!r}, error: {admin_app.error_text()!r}"
    )

    admin_app.admin.set_mail_field(weight, str(target))
    admin_app.admin.save_mail_spam()

    client = _admin_client(nest_instance)
    with client:
        # Ground truth: the Admin read twin reflects the persisted override — the
        # additive Tier-2 field rides the same put_spam_policy full-PUT.
        assert _wait(
            lambda: client.call("fauna.bridges.get_mail_config", {})["spam"][
                "bayesian_weight_milli"
            ]
            == target
        ), (
            f"put_spam_policy did not persist bayesian_weight_milli={target} "
            f"(get_mail_config still shows the old value). error: {admin_app.error_text()!r}"
        )

    # And the page re-rendered the persisted value (round-tripped, not just kept
    # the typed text).
    assert _wait(lambda: admin_app.admin.mail_field_text(weight).strip() == str(target)), (
        "the page did not re-render the persisted Bayesian weight (850). "
        f"error: {admin_app.error_text()!r}"
    )
    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid save: {admin_app.error_text()!r}"
    )


@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_auto_enable_new_users_toggle_round_trips(admin_app, nest_instance):
    """Flipping the deployment-wide "auto-enable mail for new users" toggle writes
    through the shared MailPolicyMachine → `fauna.bridges.set_auto_enable_mail_for_new_users`
    → nest, proven by reading `fauna.setup.status` over the wire (the read twin the
    toggle hydrates from — this knob is NOT in `get_mail_config`). It defaults ON
    (the works-out-of-box invariant extended to every user); we flip it to the
    opposite of whatever the nest currently holds.

    The flip is asserted as `not before` rather than a literal OFF: this knob is
    one value on a session-scoped nest shared by every app parametrization, and
    this very test flips it — so only the FIRST app to run finds it at the ON
    default (`_nest_now`)."""
    toggle = "admin-mail-auto-enable-new-users-toggle"
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )
    assert _wait(lambda: admin_app.admin.mail_field_present(toggle)), (
        f"{toggle} missing. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        # Ground truth before: whatever the nest holds (ON on a fresh nest, since
        # unset ⇒ ON — but a sibling app's run of this test may have flipped it).
        before = client.call("fauna.setup.status", {})["auto_enable_mail_for_new_users"]
        assert isinstance(before, bool), (
            f"setup.status returned a non-boolean auto_enable_mail_for_new_users: {before!r}"
        )

        # Flip it via the UI.
        admin_app.admin.toggle_mail_auto_enable_new_users()

        # Ground truth after: setup.status reflects the persisted flip.
        assert _wait(
            lambda: client.call("fauna.setup.status", {})["auto_enable_mail_for_new_users"]
            is (not before)
        ), (
            f"set_auto_enable_mail_for_new_users did not persist {not before} "
            f"(setup.status still {before}). error: {admin_app.error_text()!r}"
        )

    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid toggle: {admin_app.error_text()!r}"
    )


@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_imap_idle_timeout_save_round_trips(admin_app, nest_instance):
    """Lowering the IMAP IDLE timeout via the UI writes through the shared
    MailPolicyMachine → `fauna.bridges.put_imap_policy` → nest, proven by reading
    the Admin `get_mail_config` twin (the same read the page hydrates from). This
    exercises the second projected group beyond spam/auth (Submission/IMAP/Outbound
    all extend the machine identically). Catalog default 1740; we set 600 (900 if
    the nest already holds 600 — see `_nest_now`)."""
    before = _nest_now(
        nest_instance,
        lambda c: c.call("fauna.bridges.get_mail_config", {})["imap"]["idle_timeout_secs"],
    )
    target = _distinct_target(before, 600, 900)

    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )

    idle = "admin-mail-imap-idle-timeout-input"
    # The IMAP group is lower on the page; assert it is built before driving it.
    assert _wait(lambda: admin_app.admin.mail_field_present(idle)), (
        f"{idle} missing — IMAP group not rendered. error: {admin_app.error_text()!r}"
    )
    # Hydrates from get_mail_config — the nest's current value (the catalog
    # default 1740 only on the first app to run this test).
    assert _wait(lambda: admin_app.admin.mail_field_text(idle).strip() == str(before)), (
        f"idle timeout did not hydrate to the nest's current value {before}; "
        f"got {admin_app.admin.mail_field_text(idle)!r}, error: {admin_app.error_text()!r}"
    )

    admin_app.admin.set_mail_field(idle, str(target))
    admin_app.admin.save_mail_imap()

    client = _admin_client(nest_instance)
    with client:
        # Ground truth: the Admin read twin reflects the persisted override.
        assert _wait(
            lambda: client.call("fauna.bridges.get_mail_config", {})["imap"][
                "idle_timeout_secs"
            ]
            == target
        ), (
            f"put_imap_policy did not persist idle_timeout_secs={target} "
            f"(get_mail_config still shows the old value). error: {admin_app.error_text()!r}"
        )

    # The page re-rendered the persisted value (proving it round-tripped).
    assert _wait(lambda: admin_app.admin.mail_field_text(idle).strip() == str(target)), (
        f"the page did not re-render the persisted IMAP idle timeout ({target}). "
        f"error: {admin_app.error_text()!r}"
    )
    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid save: {admin_app.error_text()!r}"
    )


@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_alias_policy_save_round_trips(admin_app, nest_instance):
    """The nest-side alias group is the only group NOT projected into
    `FetchConfigReply` — it hydrates from a *separate* `fauna.bridges.get_alias_policy`
    admin read twin and saves via `put_alias_policy`. Lowering the per-account
    exact-alias cap via the UI writes through the shared MailPolicyMachine, proven
    by reading the `get_alias_policy` twin (NOT get_mail_config — alias is the
    dual-read group). Catalog default 20; we set 3 (5 if the nest already holds 3
    — see `_nest_now`). The alias group is lifted on web/linux/windows/macos/ios
    (module `pytestmark` above) — android is the only client still owing it."""
    before = _nest_now(
        nest_instance,
        lambda c: c.call("fauna.bridges.get_alias_policy", {})["exact_aliases_max"],
    )
    target = _distinct_target(before, 3, 5)

    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )

    # The alias group is the last group on the page; assert its controls render.
    for element_id in (
        "admin-mail-alias-exact-max-input",
        "admin-mail-alias-reserved-local-parts",
        "admin-mail-alias-subaddressing-toggle",
        "admin-mail-alias-wildcard-prefix-toggle",
        "admin-mail-alias-save-button",
    ):
        assert _wait(lambda eid=element_id: admin_app.admin.mail_field_present(eid)), (
            f"{element_id} missing — alias group not rendered. "
            f"error: {admin_app.error_text()!r}"
        )

    cap = "admin-mail-alias-exact-max-input"
    # Hydrates from the get_alias_policy twin — the nest's current value (the
    # catalog default 20 only on the first app to run this test).
    assert _wait(lambda: admin_app.admin.mail_field_text(cap).strip() == str(before)), (
        f"exact-alias cap did not hydrate to the nest's current value {before}; "
        f"got {admin_app.admin.mail_field_text(cap)!r}, error: {admin_app.error_text()!r}"
    )

    admin_app.admin.set_mail_field(cap, str(target))
    admin_app.admin.save_mail_alias()

    client = _admin_client(nest_instance)
    with client:
        # Ground truth: the dedicated alias read twin reflects the persisted
        # override (alias policy is NOT in get_mail_config's FetchConfigReply).
        assert _wait(
            lambda: client.call("fauna.bridges.get_alias_policy", {})[
                "exact_aliases_max"
            ]
            == target
        ), (
            f"put_alias_policy did not persist exact_aliases_max={target} "
            f"(get_alias_policy still shows the old value). error: {admin_app.error_text()!r}"
        )

    # The page re-rendered the persisted value via the dual-read refresh.
    assert _wait(lambda: admin_app.admin.mail_field_text(cap).strip() == str(target)), (
        f"the page did not re-render the persisted exact-alias cap ({target}). "
        f"error: {admin_app.error_text()!r}"
    )
    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid save: {admin_app.error_text()!r}"
    )


@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("admin-mail-policy")
def test_auth_max_conn_per_ip_save_round_trips(admin_app, nest_instance):
    """The per-IP **concurrent**-connection cap (the seventh `AuthPolicy` field,
    `mail.auth.per_ip_max_concurrent_conn` / wire `AuthPolicy.max_conn_per_ip`,
    mail-policy-config.md § Policy catalog) round-trips through the shared
    MailPolicyMachine → `fauna.bridges.put_auth_policy` → nest, proven by reading
    the Admin `get_mail_config` twin. Catalog default 256; we set 128 (64 if the
    nest already holds 128 — see `_nest_now`).

    **linux is the lead app** for this widget — `admin-mail-auth-max-conn-per-ip-input`
    landed on linux (lead) then **android** (`AdminMailScreen.kt` AuthGroup) and **web**
    (`admin/mail/+page.svelte` Auth group, the `maxConnPerIp` text input). windows/macos/ios
    remain. Each app adds its own `@pytest.mark.<client>` here when its lift of the widget
    lands (android's APK/emulator e2e is host-gated, so its mark only fires once that runs;
    web runs now). Runs `--client {linux,web}` (the lifted clients)."""
    before = _nest_now(
        nest_instance,
        lambda c: c.call("fauna.bridges.get_mail_config", {})["auth"]["max_conn_per_ip"],
    )
    target = _distinct_target(before, 128, 64)

    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )

    cap = "admin-mail-auth-max-conn-per-ip-input"
    # The cap lives in the auth group (below spam); assert it is built first.
    assert _wait(lambda: admin_app.admin.mail_field_present(cap)), (
        f"{cap} missing — auth group not rendered. error: {admin_app.error_text()!r}"
    )
    # Hydrates from get_mail_config — the nest's current value (the catalog
    # default 256 only on the first app to run this test).
    assert _wait(lambda: admin_app.admin.mail_field_text(cap).strip() == str(before)), (
        f"per-IP conn cap did not hydrate to the nest's current value {before}; "
        f"got {admin_app.admin.mail_field_text(cap)!r}, error: {admin_app.error_text()!r}"
    )

    admin_app.admin.set_mail_field(cap, str(target))
    admin_app.admin.save_mail_auth()

    client = _admin_client(nest_instance)
    with client:
        # Ground truth: the Admin read twin reflects the persisted override.
        assert _wait(
            lambda: client.call("fauna.bridges.get_mail_config", {})["auth"][
                "max_conn_per_ip"
            ]
            == target
        ), (
            "put_auth_policy did not persist max_conn_per_ip=128 "
            f"(get_mail_config still shows the old value). error: {admin_app.error_text()!r}"
        )

    # The page re-rendered the persisted value (proving it round-tripped).
    assert _wait(lambda: admin_app.admin.mail_field_text(cap).strip() == str(target)), (
        "the page did not re-render the persisted per-IP conn cap (128). "
        f"error: {admin_app.error_text()!r}"
    )
    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid save: {admin_app.error_text()!r}"
    )
