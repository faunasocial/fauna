"""tier_3 e2e: a nest admin's published rules, written in the shared rule editor,
bind what a member reads through the nest — visibly, naming the nest and the reason.

``nest-rules.md`` § What the build owes in tests (tier_3, through the UI on tui
first): an admin's rule blocks a post visibly with the nest named and the reason
shown; a member cannot loosen it; a rule change reaches a connected member as a
notice. Convention 17: a nest ``Block`` verdict never renders silent.

One journey on the one driver, identities switched through the shared session
helpers:

author posts a body carrying a unique term → the admin adds a keywords rule on
that term in the shared editor on ``admin-nest`` (``admin-nest-rules-section``),
writes the reason, saves, and the status names version 1 → a member's feed
paints the nest placeholder in place of that post's body — the frame naming the
nest, the domain, the admin's reason verbatim — with no reveal (a ``block`` has
no member override), and the clean post untouched → the member's settings
landing shows the rules in force at version 1 → the admin saves a second rule,
and the connected admin seat — itself a member of the nest — gets the one-line
changed notice through the nest's ``info_changed`` push, its rules surface
moving to version 2.

The changed notice adds no element id (``nest-rules.md`` § Element IDs); tui
publishes it as the additive ``nest_rules_changed_notice`` state field.

Mutations are UI-only (convention 8): the posts, both rules writes. Waits are
named budgets over deadline polls (convention 14).

App arm: **tui** (the lead app); the other six join their trickle-down by
extending ``_BUILT_APPS``.

A second journey witnesses the guardian's mounting: a category rule written
in ``family-policy-rules-section`` binds the ward's render.

Not yet witnessed here (each captured on the row that builds it): the rules at
joining, and the nest's public page.
"""

import time
import uuid

import pytest

from common.auth import create_actor_and_register
from helpers.app_surface import app_name, skip_unbuilt
from helpers.e2e_session import E2E_LOGIN_DEVICE_ID, login_as
from helpers.mail_dedicated_nest import login_as_nest_admin
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
]

_BUILT_APPS = ("tui", "web", "linux", "android")
# The apps that paint a guardian rule's reason beside the family-policy frame
# and on its summary line (`nest-rules.md` § The shared rule editor → *The
# reason is the member's to read*); tui leads, each trickle-down adds its app.
_GUARDIAN_REASON_APPS = ("tui", "web", "linux")

REASON = "No gambling promotion on this nest."
SECOND_REASON = "No lottery promotion on this nest."

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # a page render after a nav
_SEND_BUDGET_S = 60.0     # one WS-RPC round-trip + the repaint
_FEED_BUDGET_S = 30.0     # a feed (re)load reflecting the change
_PUSH_BUDGET_S = 60.0     # a push → the self-description re-read → the repaint


def _poll(check, budget_s, tag, diagnose=None):
    """Deadline-poll ``check`` until truthy; the failure names the budget and,
    when given, what the surface showed instead (convention 6)."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    seen = f" — saw: {diagnose()!r}" if diagnose else ""
    raise AssertionError(f"{tag}: not reached within {budget_s}s{seen}")


def _actor(nest_instance):
    return create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )


def _login(app, nest_instance, actor):
    login_as(
        app,
        nest_instance,
        actor,
        handle=actor["handle"],
        device_id=E2E_LOGIN_DEVICE_ID,
        wait_for_id="feed-tab",
    )


def _general_feed(app):
    app.feed.navigate()
    app.feed.open_feed("General")


def _write_keyword_rule(app, term, reason):
    """Add one keywords rule in the admin's mounting and fill it — through the
    editor's own controls, every label the shared crate's."""
    editor = app.rule_editor
    row = editor.add_rule()
    editor.pick(row, "kind", S.rule_editor.kind.keywords)
    _poll(
        lambda: editor.has_control(row, "rule-editor-item-keywords-input"),
        _PAGE_BUDGET_S,
        "a keywords rule paints its keywords input",
    )
    assert not editor.has_control(row, "rule-editor-item-level-select"), (
        "a keywords rule shows no level"
    )
    assert not editor.save_enabled(), "a rule with no subject and no reason cannot be saved"
    editor.type_input(row, "keywords", term)
    editor.type_input(row, "reason", reason)
    _poll(
        editor.save_enabled,
        _PAGE_BUDGET_S,
        "a filled rule becomes saveable",
        diagnose=editor.status,
    )


def _rules_surface(app):
    """Open the settings landing (the region section's home) and return the
    rules surface's version text, or ``None`` while it is absent."""
    app.settings.navigate()
    app.settings._navigate_subpage("status")
    if app.driver.count("settings-nest-rules-section") == 0:
        return None
    return app.driver.get_text("settings-nest-rules-version")


def _assert_block_render_witness(app, where):
    """Convention 17's nest twin, read mid-journey (the post-frame sampler sees
    only a test's last frame)."""
    counts = (app.driver.get_state() or {}).get("nest_block_render")
    assert isinstance(counts, dict), f"{where}: nest_block_render missing: {counts!r}"
    blocked, placeholders = counts.get("blocked"), counts.get("placeholders")
    assert blocked >= 1, f"{where}: a block is on screen but none is counted: {counts!r}"
    assert placeholders >= blocked, (
        f"{where}: a nest-blocked item rendered silently (placeholders < blocked): {counts!r}"
    )


def test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced(app, nest_instance):
    if app_name(app.driver) not in _BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="admin-nest-rules-section",
            detail="the nest rules (nest-rules.md § The shared rule editor) are built "
            "on tui first",
            tracked="",
        )

    token = uuid.uuid4().hex[:10]
    term = f"casino{token}"
    target_text = f"tonight at the {term} hall"
    clean_text = f"an ordinary post {token}"
    author, member = _actor(nest_instance), _actor(nest_instance)

    # The author posts both bodies through the app.
    _login(app, nest_instance, author)
    app.feed.navigate()
    app.feed.create_post(clean_text)
    app.feed.create_post(target_text)
    row = app.feed.wait_for_post_state_by_text(target_text, timeout=_FEED_BUDGET_S)
    assert row is not None, f"the target post must land; error={app.error_text()!r}"
    post_id = row["post_id"]
    clean = app.feed.wait_for_post_state_by_text(clean_text, timeout=_FEED_BUDGET_S)
    assert clean is not None, f"the clean post must land; error={app.error_text()!r}"

    # The admin writes the rule in the shared editor on admin-nest.
    login_as_nest_admin(app, nest_instance, nest_instance["url"])
    app.admin.navigate_nest()
    _poll(
        lambda: app.driver.count("admin-nest-rules-section") > 0,
        _PAGE_BUDGET_S,
        "the admin Nest page paints its rules section",
        diagnose=lambda: app.error_text() if app.has_error() else "no error",
    )
    app.rule_editor.wait_loaded()
    _write_keyword_rule(app, term, REASON)
    app.rule_editor.save()
    app.rule_editor.wait_status(S.rule_editor.saved_version(sequence="1"), timeout=_SEND_BUDGET_S)

    # The member reads the nest's feed: the placeholder stands in place of the
    # matching post's body, naming the nest and the admin's reason.
    _login(app, nest_instance, member)
    _general_feed(app)
    assert app.feed.wait_for_post_text(clean_text, timeout_s=_FEED_BUDGET_S), (
        f"the member must see the clean post; error={app.error_text()!r}"
    )

    def _card():
        idx = app.feed.post_index_by_id(post_id)
        return f"post-card[{idx}]" if idx >= 0 else None

    _poll(
        lambda: _card() is not None
        and app.driver.count("nest-blocked-notice", scope=_card()) == 1,
        _FEED_BUDGET_S,
        "the matching post paints the nest placeholder",
        diagnose=lambda: (_card(), app.driver.diagnose("nest-blocked-notice")),
    )
    scope = _card()
    domain = app.driver.get_text("nest-blocked-domain", scope=scope)
    assert domain, "the placeholder names the nest's domain"
    assert app.driver.get_text("nest-blocked-notice", scope=scope) == (
        S.nest_rules.blocked_notice(nest=domain)
    )
    assert app.driver.get_text("nest-blocked-reason", scope=scope) == REASON
    assert app.driver.count("feed-post-text", scope=scope) == 0, "a blocked post renders no body"
    # Never loosenable: a nest block offers the member no reveal.
    assert app.driver.count("nest-collapsed-reveal-button", scope=scope) == 0
    clean_scope = f"post-card[{app.feed.post_index_by_id(clean['post_id'])}]"
    assert app.driver.count("nest-blocked-notice", scope=clean_scope) == 0
    assert app.driver.count("feed-post-text", scope=clean_scope) >= 1
    _assert_block_render_witness(app, "the member's feed")

    # The member's rules surface names the rules in force.
    version = _rules_surface(app)
    assert version is not None and version.startswith("Version 1,"), (
        f"the rules surface names version 1: {version!r}"
    )
    assert app.driver.count("settings-nest-rules-item") == 1
    assert REASON in app.driver.get_text("settings-nest-rules-item", index=0)

    # A change: the admin adds a second rule. The admin seat, connected and a
    # member of the nest itself, gets the nest's push and the one-line notice.
    login_as_nest_admin(app, nest_instance, nest_instance["url"])
    app.feed.navigate()
    app.admin.navigate_nest()
    app.rule_editor.wait_loaded()
    _poll(
        lambda: app.rule_editor.rule_count() == 1,
        _PAGE_BUDGET_S,
        "the editor loads the rule in force",
        diagnose=app.rule_editor.status,
    )
    _write_keyword_rule(app, f"lottery{token}", SECOND_REASON)
    app.rule_editor.save()
    app.rule_editor.wait_status(S.rule_editor.saved_version(sequence="2"), timeout=_SEND_BUDGET_S)
    _poll(
        lambda: (app.driver.get_state() or {}).get("nest_rules_changed_notice")
        == S.nest_rules.changed_notice(nest=domain),
        _PUSH_BUDGET_S,
        "the connected seat gets the changed notice",
        diagnose=lambda: (app.driver.get_state() or {}).get("nest_rules_changed_notice"),
    )
    _poll(
        lambda: (_rules_surface(app) or "").startswith("Version 2,"),
        _PUSH_BUDGET_S,
        "the rules surface moves to version 2",
        diagnose=lambda: _rules_surface(app),
    )
    assert app.driver.count("settings-nest-rules-item") == 2
    assert (app.driver.get_state() or {}).get("nest_rules_changed_notice") is None, (
        "opening the rules surface reads the change"
    )


def test_a_guardians_category_rule_written_in_the_shared_editor_binds_the_wards_render(
    app, request, nest_instance
):
    """The guardian's mounting (``nest-rules.md`` § The shared rule editor (b);
    ``family-safety.md`` § App surface → *Content rules*): the guardian adds a
    category rule in ``family-policy-rules-section`` for the selected ward —
    category spam, verdict block, a reason — and saves; the editor reloads the
    rule from what the nest stored; the ward's own render then hides a
    spam-labelled post behind the content-policy placeholder while a clean
    post renders. ``test_family.py``'s content arms drive the same editor on
    tui through ``FamilyActions.set_content_floor``."""
    if app_name(app.driver) not in _BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="family-policy-rules-section",
            detail="the guardian's rule editor (nest-rules.md § The shared rule editor (b)) "
            "is built on tui first",
            tracked="",
        )
    from conftest import _login_app_as
    from helpers.family_wire import admit_adult_directly, admit_ward

    suffix = uuid.uuid4().hex[:6]
    ward_handle = f"rules-ward-{suffix}"
    guardian = admit_adult_directly(nest_instance, f"rules-guardian-{suffix}")
    ward = admit_ward(nest_instance, guardian, ward_handle)
    reason = "Not for this account"

    # The guardian writes the rule in the shared editor on the family page.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian)
    app.family.navigate()

    def _ward_listed():
        if ward_handle in app.family.ward_handles():
            return True
        app.family.reload()
        return False

    _poll(_ward_listed, _SEND_BUDGET_S, "the guardian's list names the ward",
          diagnose=lambda: app.error_text() if app.has_error() else "no error")
    app.family.select_ward_by_handle(ward_handle)
    _poll(
        lambda: app.driver.count("family-policy-rules-section") > 0,
        _PAGE_BUDGET_S,
        "the family page paints its rules section",
    )
    editor = app.rule_editor
    editor.wait_loaded()
    assert editor.rule_count() == 0, "a new ward starts with no rules"
    assert app.driver.count("family-policy-content-spam-select") == 0, (
        "the rule editor stands where the floor selects stood"
    )
    row = editor.add_rule()
    editor.pick(row, "category", S.moderation.category.spam)
    editor.pick(row, "verdict", S.rule_editor.verdict.block)
    assert editor.picked(row, "level") == S.rule_editor.level.standard
    editor.type_input(row, "reason", reason)
    _poll(editor.save_enabled, _PAGE_BUDGET_S, "the filled rule becomes saveable",
          diagnose=editor.status)
    editor.save()
    editor.wait_status(S.rule_editor.saved, timeout=_SEND_BUDGET_S)
    # What paints after the save is what the nest stored.
    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    editor.wait_loaded()
    _poll(lambda: editor.rule_count() == 1, _PAGE_BUDGET_S, "the stored rule reloads",
          diagnose=editor.status)
    assert editor.picked(0, "category") == S.moderation.category.spam
    assert editor.picked(0, "verdict") == S.rule_editor.verdict.block

    # The ward's own render binds. First the ward's device holds the document
    # (tui's additive `ward_rule_document_rules` state field), so a red below
    # names its own layer: the read, or the render (convention 6).
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward)
    app.driver.wait_for("supervised-indicator", timeout=_PAGE_BUDGET_S)

    def _held_rules():
        return (app.driver.get_state() or {}).get("ward_rule_document_rules")

    _poll(
        lambda: _held_rules() == 1,
        _SEND_BUDGET_S,
        "the ward's device holds the guardian's one-rule document",
        diagnose=_held_rules,
    )
    count = app.feed.seed_posts([
        {"post_id": f"gr-clean-{suffix}", "author": "afriend", "body": "an ordinary post"},
        {
            "post_id": f"gr-spam-{suffix}",
            "author": "aspammer",
            "body": "this body must never render",
            "labels": [{"category": "spam", "confidence_per_mille": 900}],
        },
    ])
    assert count == 2, f"expected 2 seeded post-cards, got {count}; error={app.error_text()!r}"
    _poll(
        lambda: app.family.content_notice_blocked("post-card[1]"),
        _FEED_BUDGET_S,
        "the guardian's rule hides the spam post",
        diagnose=lambda: (
            app.driver.diagnose("content-policy-blocked-notice"),
            _held_rules(),
        ),
    )
    assert not app.family.content_notice_blocked("post-card[0]"), "the clean post renders"
    paints_reason = app_name(app.driver) in _GUARDIAN_REASON_APPS
    if paints_reason:
        # The reason is the ward's to read (`nest-rules.md` § The shared rule
        # editor): the typed reason verbatim beside the frame, in the same
        # card; the notice's own text stays the app's fixed frame.
        assert app.family.content_notice_reason("post-card[1]") == reason, (
            f"the hidden card shows the guardian's reason verbatim; got "
            f"{app.family.content_notice_reason('post-card[1]')!r}"
        )
        assert app.family.content_notice_text("post-card[1]") == S.family.content_blocked_notice, (
            "the notice keeps the fixed frame; the reason is beside it, never inside"
        )
        assert app.family.content_notice_reason("post-card[0]") is None

    # Transparency: the ward's read-only summary names the rule that binds them.
    app.family.navigate()

    def _summary():
        if app.driver.count("family-policy-summary") == 0:
            return ""
        return app.driver.get_text("family-policy-summary") or ""

    def _spam_line():
        return next(
            (
                line
                for line in _summary().splitlines()
                if S.family.policy_content_spam_label in line and S.family.value_block in line
            ),
            None,
        )

    _poll(
        lambda: _spam_line() is not None,
        _PAGE_BUDGET_S,
        "the ward's summary names the guardian's spam rule",
        diagnose=_summary,
    )
    if paints_reason:
        assert _spam_line().endswith(f" — {reason}"), (
            f"the rule's summary line carries its reason; got {_spam_line()!r}"
        )
