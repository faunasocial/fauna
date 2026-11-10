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
writes the reason, saves, and the status names the version it reached → a member's feed
paints the nest placeholder in place of that post's body — the frame naming the
nest, the domain, the admin's reason verbatim — with no reveal (a ``block`` has
no member override), and the clean post untouched → the member's settings
landing shows the rules in force at that version → the admin saves a second rule,
and the connected admin seat — itself a member of the nest — gets the one-line
changed notice through the nest's ``info_changed`` push, its rules surface
moving to the next version. The session's nest keeps an earlier
app's rules, so the journey counts from the rule book it finds.

The changed notice adds no element id (``nest-rules.md`` § Element IDs); tui
publishes it as the additive ``nest_rules_changed_notice`` state field.

Mutations are UI-only (convention 8): the posts, both rules writes. Waits are
named budgets over deadline polls (convention 14).

App arm: every app (tui led; windows, the seventh, joined 2026-11-04 — the
surface gates retired with it).

The guardian's mounting (``family-safety`` outcomes 56–58): a category rule
written in ``family-policy-rules-section`` binds the ward's render with the
guardian's reason beside the block, and a collapse rule beside the collapse,
each on its summary line too; a keywords rule and a labeler rule bind on the
ward's device like a category rule; and a guardian document this build cannot
read (stored through the nest's ``test-hooks`` endpoint, since the write path
refuses one) withholds every canonical category rather than reading as absent.

A third journey witnesses joining as acceptance: a new identity's
``invite_request`` and an admin-admitted identity's ``handle_entry`` (once the
real handle check concludes Continue signs in) show the published rules beside
the terms notice.

Two more: an admin's ``prompt`` rule, which no app runs yet, is listed as not
applied on this device in Settings and, under its line, on ``invite_request``;
and a nest ``collapse`` rule folds the matching post behind the placeholder's
reveal, which shows the body.

Not yet witnessed here (captured on the row that builds it): the nest's public
page.
"""

import time
import uuid

import pytest

from common.auth import create_actor_and_register
from helpers.app_surface import app_name
from helpers.e2e_session import e2e_login_device_id, login_as
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

# The apps that paint a guardian rule's reason beside the family-policy frame
# and on its summary line (`nest-rules.md` § The shared rule editor → *The
# reason is the member's to read*); tui leads, each trickle-down adds its app.
_GUARDIAN_REASON_APPS = ("tui", "web", "linux", "macos", "ios", "windows")

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


def _dial_url(app, nest_instance, request):
    """The raw nest on a native app; web's SPA proxy onto it on web
    (single-origin — the raw nest is a CORS refusal)."""
    return request.getfixturevalue("spa_url") if app.driver.is_web() else nest_instance["url"]


def _login(app, nest_instance, actor, request):
    login_as(
        app,
        nest_instance,
        actor,
        handle=actor["handle"],
        device_id=e2e_login_device_id(app),
        request=request,
        spa_url_fixture="spa_url",
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


def _saved_sequence(app):
    """Wait for the editor's save to land and return the version it names.
    The session's nest keeps the rule book an earlier app's run published, so
    the journey reads the version it reached instead of assuming a fresh nest."""
    prefix = S.rule_editor.saved_version(sequence="")
    _poll(
        lambda: app.rule_editor.status().startswith(prefix)
        and app.rule_editor.status()[len(prefix):].isdigit(),
        _SEND_BUDGET_S,
        "the admin's rule is saved",
        diagnose=app.rule_editor.status,
    )
    return int(app.rule_editor.status()[len(prefix):])


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


@pytest.mark.feature("nest-rules")
def test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced(
    app, nest_instance, request
):
    dial_url = _dial_url(app, nest_instance, request)

    token = uuid.uuid4().hex[:10]
    term = f"casino{token}"
    target_text = f"tonight at the {term} hall"
    clean_text = f"an ordinary post {token}"
    author, member = _actor(nest_instance), _actor(nest_instance)

    # The author posts both bodies through the app.
    _login(app, nest_instance, author, request)
    app.feed.navigate()
    app.feed.create_post(clean_text)
    app.feed.create_post(target_text)
    row = app.feed.wait_for_post_state_by_text(target_text, timeout=_FEED_BUDGET_S)
    assert row is not None, f"the target post must land; error={app.error_text()!r}"
    post_id = row["post_id"]
    clean = app.feed.wait_for_post_state_by_text(clean_text, timeout=_FEED_BUDGET_S)
    assert clean is not None, f"the clean post must land; error={app.error_text()!r}"

    # The admin writes the rule in the shared editor on admin-nest.
    login_as_nest_admin(app, nest_instance, dial_url)
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
    sequence = _saved_sequence(app)
    # The rules in force: an earlier app's run on this session's nest included.
    in_force = app.rule_editor.rule_count()

    # The member reads the nest's feed: the placeholder stands in place of the
    # matching post's body, naming the nest and the admin's reason.
    _login(app, nest_instance, member, request)
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
    clean_scope = f"post-card[{app.feed.card_index_by_id(clean['post_id'])}]"
    assert app.driver.count("nest-blocked-notice", scope=clean_scope) == 0
    assert app.driver.count("feed-post-text", scope=clean_scope) >= 1
    _assert_block_render_witness(app, "the member's feed")

    # The member's rules surface names the rules in force.
    version = _rules_surface(app)
    assert version is not None and version.startswith(f"Version {sequence},"), (
        f"the rules surface names version {sequence}: {version!r}"
    )
    items = app.driver.count("settings-nest-rules-item")
    assert items == in_force, f"the surface lists the {in_force} rules in force: {items}"
    assert any(
        REASON in app.driver.get_text("settings-nest-rules-item", index=i) for i in range(items)
    ), "the surface lists the admin's rule"

    # A change: the admin adds a second rule. The admin seat, connected and a
    # member of the nest itself, gets the nest's push and the one-line notice.
    login_as_nest_admin(app, nest_instance, dial_url)
    app.feed.navigate()
    app.admin.navigate_nest()
    app.rule_editor.wait_loaded()
    _poll(
        lambda: app.rule_editor.rule_count() == in_force,
        _PAGE_BUDGET_S,
        "the editor loads the rules in force",
        diagnose=app.rule_editor.status,
    )
    _write_keyword_rule(app, f"lottery{token}", SECOND_REASON)
    app.rule_editor.save()
    app.rule_editor.wait_status(
        S.rule_editor.saved_version(sequence=str(sequence + 1)), timeout=_SEND_BUDGET_S
    )
    _poll(
        lambda: (app.driver.get_state() or {}).get("nest_rules_changed_notice")
        == S.nest_rules.changed_notice(nest=domain),
        _PUSH_BUDGET_S,
        "the connected seat gets the changed notice",
        diagnose=lambda: (app.driver.get_state() or {}).get("nest_rules_changed_notice"),
    )
    _poll(
        lambda: (_rules_surface(app) or "").startswith(f"Version {sequence + 1},"),
        _PUSH_BUDGET_S,
        f"the rules surface moves to version {sequence + 1}",
        diagnose=lambda: _rules_surface(app),
    )
    assert app.driver.count("settings-nest-rules-item") == in_force + 1
    assert (app.driver.get_state() or {}).get("nest_rules_changed_notice") is None, (
        "opening the rules surface reads the change"
    )


# ── The guardian's mounting (`nest-rules.md` § The shared rule editor (b)) ──
#
# Each journey admits its own guardian and ward, writes the guardian's rules
# in `family-policy-rules-section` through the editor's own controls, then
# signs in as the ward and reads the ward's own render. What the ward's device
# holds is read first (tui's additive `ward_rule_document_rules` state field),
# so a red names its own layer: the read, or the render (convention 6).

# The committed cat detector (`fixtures/labeler/cat_labeler.wat`): one `cat`
# label at 900 per-mille on any input carrying the bytes "cat", nothing
# otherwise.
_CAT_WAT_NAME = "cat_labeler.wat"

# A guardian rule document at a grammar version no build implements: the
# canonical CBOR map `{"version": 99}`. Stored through the test hook, since
# `fauna.family.policy.update` refuses it (a nest never stores rules no device
# on its build could apply).
_UNREADABLE_DOCUMENT_HEX = "a16776657273696f6e1863"


def _guardian_and_ward(nest_instance, tag):
    """A fresh guardian and a ward admitted under them. Returns
    ``(guardian, ward, ward_handle, suffix)``."""
    from helpers.admin_wire import admit_adult
    from helpers.family_wire import admit_ward

    suffix = uuid.uuid4().hex[:6]
    ward_handle = f"{tag}-ward-{suffix}"
    guardian = admit_adult(nest_instance, f"{tag}-guardian-{suffix}")
    ward = admit_ward(nest_instance, guardian, ward_handle)
    return guardian, ward, ward_handle, suffix


def _select_ward(app, ward_handle):
    """On the guardian's family page: wait for the ward to list, select it, and
    wait for the shared editor to load its document."""

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
    app.rule_editor.wait_loaded()


def _open_wards_editor(app, request, nest_instance, guardian, ward_handle):
    """Sign in as the guardian and open the ward's rules in the shared editor;
    a new ward starts with none."""
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian)
    app.family.navigate()
    _select_ward(app, ward_handle)
    assert app.rule_editor.rule_count() == 0, "a new ward starts with no rules"
    return app.rule_editor


def _fill_reason_and_save(app, ward_handle, row, reason, rules):
    """Type the rule's reason, save, and wait until what the editor paints
    after a reload is what the nest stored: ``rules`` rules."""
    editor = app.rule_editor
    editor.type_input(row, "reason", reason)
    _poll(editor.save_enabled, _PAGE_BUDGET_S, "the filled rule becomes saveable",
          diagnose=editor.status)
    editor.save()
    editor.wait_status(S.rule_editor.saved, timeout=_SEND_BUDGET_S)
    app.family.reload()
    _select_ward(app, ward_handle)
    _poll(lambda: editor.rule_count() == rules, _PAGE_BUDGET_S, "the stored rule reloads",
          diagnose=editor.status)


def _ward_holds(app, request, nest_instance, ward, rules, what):
    """Sign in as the ward and wait until its device holds a guardian
    document of ``rules`` rules."""
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward)
    app.driver.wait_for("supervised-indicator", timeout=_PAGE_BUDGET_S)
    _poll(
        lambda: _held_rules(app) == rules,
        _SEND_BUDGET_S,
        f"the ward's device holds {what}",
        diagnose=lambda: (_held_rules(app), _policy_sources(app)),
    )


def _held_rules(app):
    return (app.driver.get_state() or {}).get("ward_rule_document_rules")


def _policy_sources(app):
    return (app.driver.get_state() or {}).get("content_policy_sources")


def _seed(app, posts):
    count = app.feed.seed_posts(posts)
    assert count == len(posts), (
        f"expected {len(posts)} seeded post-cards, got {count}; error={app.error_text()!r}"
    )


def _assert_withheld_with_reason(app, scope, reason, *, collapsed=False):
    """``scope`` paints the family-policy placeholder in place of its body —
    blocked, or collapsed with the reveal — and, on an app that paints it,
    the guardian's reason verbatim beside the app's fixed frame."""
    if collapsed:
        _poll(
            lambda: app.driver.count("content-policy-blocked-notice", scope=scope) == 1
            and app.driver.get_attr("content-policy-blocked-notice", "state", scope=scope)
            == "collapsed",
            _FEED_BUDGET_S,
            "the guardian's rule collapses the matching post",
            diagnose=lambda: (
                app.driver.diagnose("content-policy-blocked-notice"),
                _policy_sources(app),
            ),
        )
        assert app.driver.count("content-policy-collapsed-reveal-button", scope=scope) == 1, (
            "a collapse offers the reveal"
        )
        frame = S.family.content_collapsed_notice
    else:
        _poll(
            lambda: app.family.content_notice_blocked(scope),
            _FEED_BUDGET_S,
            "the guardian's rule hides the matching post",
            diagnose=lambda: (
                app.driver.diagnose("content-policy-blocked-notice"),
                _policy_sources(app),
            ),
        )
        frame = S.family.content_blocked_notice
    if app_name(app.driver) in _GUARDIAN_REASON_APPS:
        # The reason is the ward's to read (`nest-rules.md` § The shared rule
        # editor): the typed reason verbatim beside the frame, in the same
        # card; the notice's own text stays the app's fixed frame.
        assert app.family.content_notice_reason(scope) == reason, (
            f"the withheld card shows the guardian's reason verbatim; got "
            f"{app.family.content_notice_reason(scope)!r}"
        )
        assert app.family.content_notice_text(scope) == frame, (
            "the notice keeps the fixed frame; the reason is beside it, never inside"
        )


def _assert_shown(app, scope):
    assert app.driver.count("content-policy-blocked-notice", scope=scope) == 0, (
        f"{scope} renders: {app.driver.diagnose('content-policy-blocked-notice')}"
    )
    assert app.family.content_notice_reason(scope) is None


def _summary(app):
    if app.driver.count("family-policy-summary") == 0:
        return ""
    return app.driver.get_text("family-policy-summary") or ""


def _assert_summary_carries(app, reason):
    """Transparency: the ward's read-only summary has a line for the rule, and
    on an app that paints it, the line ends with the rule's reason."""
    app.family.navigate()
    if app_name(app.driver) not in _GUARDIAN_REASON_APPS:
        return
    _poll(
        lambda: any(line.endswith(f" — {reason}") for line in _summary(app).splitlines()),
        _PAGE_BUDGET_S,
        "the ward's summary line for the rule carries its reason",
        diagnose=lambda: _summary(app),
    )


@pytest.mark.feature("family-safety")
def test_a_guardians_category_rule_written_in_the_shared_editor_binds_the_wards_render(
    app, request, nest_instance
):
    """The guardian's mounting (``nest-rules.md`` § The shared rule editor (b);
    ``family-safety.md`` § App surface → *Content rules*): the guardian adds a
    category rule in ``family-policy-rules-section`` for the selected ward —
    category spam, verdict block, a reason — and saves; the editor reloads the
    rule from what the nest stored; the ward's own render then hides a
    spam-labelled post behind the content-policy placeholder with the
    guardian's reason beside it, while a clean post renders, and the ward's
    summary line for the rule carries the reason. ``test_family.py``'s content
    arms drive the same editor on tui through
    ``FamilyActions.set_content_floor``."""
    guardian, ward, ward_handle, suffix = _guardian_and_ward(nest_instance, "rules")
    reason = "Not for this account"

    editor = _open_wards_editor(app, request, nest_instance, guardian, ward_handle)
    row = editor.add_rule()
    editor.pick(row, "category", S.moderation.category.spam)
    editor.pick(row, "verdict", S.rule_editor.verdict.block)
    assert editor.picked(row, "level") == S.rule_editor.level.standard
    _fill_reason_and_save(app, ward_handle, row, reason, 1)
    assert editor.picked(0, "category") == S.moderation.category.spam
    assert editor.picked(0, "verdict") == S.rule_editor.verdict.block

    _ward_holds(app, request, nest_instance, ward, 1, "the guardian's one-rule document")
    _seed(app, [
        {"post_id": f"gr-clean-{suffix}", "author": "afriend", "body": "an ordinary post"},
        {
            "post_id": f"gr-spam-{suffix}",
            "author": "aspammer",
            "body": "this body must never render",
            "labels": [{"category": "spam", "confidence_per_mille": 900}],
        },
    ])
    _assert_withheld_with_reason(app, "post-card[1]", reason)
    _assert_shown(app, "post-card[0]")

    # The summary names the spam rule, and its line carries the reason.
    app.family.navigate()

    def _spam_line():
        return next(
            (
                line
                for line in _summary(app).splitlines()
                if S.family.policy_content_spam_label in line and S.family.value_block in line
            ),
            None,
        )

    _poll(
        lambda: _spam_line() is not None,
        _PAGE_BUDGET_S,
        "the ward's summary names the guardian's spam rule",
        diagnose=lambda: _summary(app),
    )
    _assert_summary_carries(app, reason)


@pytest.mark.feature("family-safety")
def test_a_guardians_collapse_rule_shows_its_reason_beside_the_collapsed_item(
    app, request, nest_instance
):
    """The reason paints on a collapse as on a block (``nest-rules.md`` § The
    shared rule editor → *The reason is the member's to read*: "on the
    placeholder (block and collapse alike) and on the read-only
    ``family-policy-summary`` line"). The category is ``nsfw``, which no
    viewer's own threshold collapses, so the collapse is the guardian's rule
    alone."""
    guardian, ward, ward_handle, suffix = _guardian_and_ward(nest_instance, "collapse")
    reason = f"Ask me first {suffix}"

    editor = _open_wards_editor(app, request, nest_instance, guardian, ward_handle)
    row = editor.add_rule()
    editor.pick(row, "category", S.moderation.category.nsfw)
    editor.pick(row, "verdict", S.rule_editor.verdict.collapse)
    _fill_reason_and_save(app, ward_handle, row, reason, 1)
    assert editor.picked(0, "verdict") == S.rule_editor.verdict.collapse

    _ward_holds(app, request, nest_instance, ward, 1, "the guardian's collapse rule")
    _seed(app, [
        {"post_id": f"gc-clean-{suffix}", "author": "afriend", "body": "an ordinary post"},
        {
            "post_id": f"gc-nsfw-{suffix}",
            "author": "astranger",
            "body": "this body stays behind the warning",
            "labels": [{"category": "nsfw", "confidence_per_mille": 900}],
        },
    ])
    _assert_withheld_with_reason(app, "post-card[1]", reason, collapsed=True)
    assert app.driver.count("feed-post-text", scope="post-card[1]") == 0, (
        "a collapsed post renders no body before the reveal"
    )
    _assert_shown(app, "post-card[0]")
    _assert_summary_carries(app, reason)


@pytest.mark.feature("family-safety")
def test_a_guardians_keyword_rule_withholds_the_matching_post_on_the_wards_device(
    app, request, nest_instance
):
    """A guardian's keywords rule binds on the ward's device like a category
    rule (``nest-rules.md`` § The shared rule editor; the ward's join,
    ``fauna_client_policy::GuardianRules``): the keyword scorer runs over the
    item's plaintext on the device, so the matching post is withheld with the
    reason and the post without the term renders."""
    guardian, ward, ward_handle, suffix = _guardian_and_ward(nest_instance, "keyword")
    term = f"casino{suffix}"
    reason = f"No gambling talk {suffix}"

    editor = _open_wards_editor(app, request, nest_instance, guardian, ward_handle)
    row = editor.add_rule()
    editor.pick(row, "kind", S.rule_editor.kind.keywords)
    _poll(
        lambda: editor.has_control(row, "rule-editor-item-keywords-input"),
        _PAGE_BUDGET_S,
        "a keywords rule paints its keywords input",
    )
    editor.type_input(row, "keywords", term)
    editor.pick(row, "verdict", S.rule_editor.verdict.block)
    _fill_reason_and_save(app, ward_handle, row, reason, 1)
    assert editor.picked(0, "kind") == S.rule_editor.kind.keywords

    _ward_holds(app, request, nest_instance, ward, 1, "the guardian's keywords rule")
    _seed(app, [
        {"post_id": f"gk-clean-{suffix}", "author": "afriend", "body": "an ordinary post"},
        {
            "post_id": f"gk-term-{suffix}",
            "author": "astranger",
            "body": f"meet me at the {term} tonight",
        },
    ])
    _assert_withheld_with_reason(app, "post-card[1]", reason)
    _assert_shown(app, "post-card[0]")
    _assert_summary_carries(app, reason)


@pytest.mark.feature("family-safety")
def test_a_guardians_labeler_rule_withholds_what_the_labeler_flags_on_the_wards_device(
    app, request, nest_instance, run_seal_helper
):
    """A guardian's labeler rule binds on the ward's device like a category
    rule: the guardian subscribes to a community labeler, names it in a
    labeler rule — the editor bundles its module into the document — and the
    ward's device runs that module over each item, withholding what it flags
    with the reason. Publishing the labeler is fixture setup (convention 8
    carve-out (b), ``helpers/labeler_publish.py``); subscribing and the rule
    are the guardian's gestures."""
    from helpers.labeler_publish import FIXTURES, new_publisher, publish_labeler_version

    guardian, ward, ward_handle, suffix = _guardian_and_ward(nest_instance, "labeler")
    reason = f"No cat pictures {suffix}"
    labeler_id, pub_seed = new_publisher(nest_instance)
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed,
        (FIXTURES / _CAT_WAT_NAME).read_bytes(),
        version=1, content_kind="post", artifact_kind="wasm",
    )
    factor = "labeler:" + labeler_id.hex()

    # The guardian subscribes, then names the labeler in a rule.
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian)
    catalog = app.labeler_catalog
    catalog.navigate_catalog()
    _poll(
        lambda: catalog.find_index_by_factor(factor) is not None,
        _SEND_BUDGET_S,
        "the published labeler lists in the guardian's catalog",
        diagnose=lambda: app.error_text() if app.has_error() else "no error",
    )
    index = catalog.find_index_by_factor(factor)
    catalog.subscribe(index)
    assert catalog.wait_for_subscribed_state(index, subscribed=True), (
        f"subscribe never flipped the row; error={app.error_text()!r}"
    )

    app.family.navigate()
    _select_ward(app, ward_handle)
    editor = app.rule_editor
    assert editor.rule_count() == 0, "a new ward starts with no rules"
    row = editor.add_rule()
    editor.pick(row, "kind", S.rule_editor.kind.labeler)
    _poll(
        lambda: editor.has_control(row, "rule-editor-item-labeler-select"),
        _PAGE_BUDGET_S,
        "a labeler rule paints its labeler picker",
    )
    # A subscribed labeler is offered under its scorer name — `labeler-` and
    # the first twelve hex digits of its id (`labeler_scorer_name`).
    editor.pick(row, "labeler", "labeler-" + labeler_id.hex()[:12])
    editor.pick(row, "verdict", S.rule_editor.verdict.block)
    _fill_reason_and_save(app, ward_handle, row, reason, 1)
    assert editor.picked(0, "kind") == S.rule_editor.kind.labeler

    _ward_holds(app, request, nest_instance, ward, 1, "the guardian's labeler rule")
    # The control carries no "cat" anywhere (its suffix is hex, which never
    # spells it).
    _seed(app, [
        {"post_id": f"gl-clean-{suffix}", "author": "afriend",
         "body": "the bus timetable changes on monday"},
        {"post_id": f"gl-cat-{suffix}", "author": "astranger",
         "body": "the cat curled up on the windowsill"},
    ])
    _assert_withheld_with_reason(app, "post-card[1]", reason)
    _assert_shown(app, "post-card[0]")
    _assert_summary_carries(app, reason)


def _store_guardian_document(nest_instance, ward_hex, document_hex):
    """The nest's ``test-hooks`` endpoint ``POST
    /api/v1/test/family/content_document``: store raw bytes as the ward's
    guardian rule document; returns the rows written."""
    import json
    import urllib.request

    body = json.dumps({"actor_id": ward_hex, "document_hex": document_hex}).encode()
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/test/family/content_document",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"content-document hook returned {resp.status}"
        return json.loads(resp.read())["written"]


@pytest.mark.feature("family-safety")
def test_a_guardian_document_this_build_cannot_read_withholds_rather_than_shows(
    app, request, nest_instance
):
    """Fail-closed (``nest-rules.md`` § Implementation status today: a guardian
    document this build cannot apply "arrives inert and renders fail-closed",
    every canonical category at block): the ward's guardian document is a
    grammar version no build implements. The ward's device still holds a
    document — never reads it as absent — and withholds every item a
    canonical category labels, with the frame alone (an unreadable document
    carries no reason), while an unlabelled post renders. The document is
    stored through the nest's test hook because the write path refuses it."""
    _guardian, ward, _ward_handle, suffix = _guardian_and_ward(nest_instance, "unread")
    written = _store_guardian_document(nest_instance, ward["actor_id_hex"],
                                       _UNREADABLE_DOCUMENT_HEX)
    assert written == 1, f"the ward's policy row takes the document: wrote {written}"

    # An unreadable document holds no rules the device can read — 0, never
    # the `None` of no document at all.
    _ward_holds(app, request, nest_instance, ward, 0, "an inert guardian document")
    _seed(app, [
        {"post_id": f"gu-clean-{suffix}", "author": "afriend", "body": "an ordinary post"},
        {
            "post_id": f"gu-nsfw-{suffix}",
            "author": "astranger",
            "body": "this body must never render",
            "labels": [{"category": "nsfw", "confidence_per_mille": 900}],
        },
        {
            "post_id": f"gu-ad-{suffix}",
            "author": "ashop",
            "body": "this body must never render either",
            "labels": [{"category": "commercial", "confidence_per_mille": 900}],
        },
    ])
    for scope in ("post-card[1]", "post-card[2]"):
        _poll(
            lambda scope=scope: app.family.content_notice_blocked(scope),
            _FEED_BUDGET_S,
            f"the unreadable document withholds {scope}",
            diagnose=lambda: (
                app.driver.diagnose("content-policy-blocked-notice"),
                _policy_sources(app),
            ),
        )
        assert app.family.content_notice_text(scope) == S.family.content_blocked_notice
        assert app.family.content_notice_reason(scope) is None, (
            "an unreadable document invents no reason: the frame alone"
        )
        assert app.driver.count("feed-post-text", scope=scope) == 0, (
            f"{scope}: a withheld post renders no body"
        )
    _assert_shown(app, "post-card[0]")

    # Transparency reads the same fail-closed policy: every canonical
    # category at block, none of it carrying a reason.
    app.family.navigate()
    labels = (
        S.family.policy_content_nsfw_label,
        S.family.policy_content_spam_label,
        S.family.policy_content_phishing_label,
        S.family.policy_content_commercial_label,
    )

    def _blocked_lines():
        return [
            line
            for line in _summary(app).splitlines()
            if any(label in line for label in labels)
            and S.family.value_block in line
        ]

    _poll(
        lambda: len(_blocked_lines()) == len(labels),
        _PAGE_BUDGET_S,
        "the ward's summary shows every category blocked",
        diagnose=lambda: _summary(app),
    )
    assert not any(" — " in line for line in _blocked_lines()), _blocked_lines()


# ── Joining is acceptance (`nest-rules.md` § Acceptance at joining) ─────────


def _admin_ws(nest_instance):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest_instance["url"], actor_id=bytes(admin.verify_key), signing_key=bytes(admin)
    )


def _admit_fresh(nest_instance) -> tuple[str, str]:
    """An identity the admin admitted directly, with no sign-in of its own —
    it never sees ``invite_request``. Returns ``(secret_hex, localpart)``."""
    import secrets

    from nacl.signing import SigningKey

    sk = SigningKey.generate()
    localpart = "joiner" + secrets.token_hex(3)
    with _admin_ws(nest_instance) as ws:
        ws.call(
            "fauna.admin.users.create",
            {
                "actor_id": bytes(sk.verify_key),
                "tier": "free",
                "label": localpart,
                "handle": localpart,
            },
        )
    return bytes(sk).hex(), localpart


def _assert_the_joining_rules(app, domain, reason, page):
    """The heading naming the nest, a rule line carrying the admin's reason,
    and the terms notice beside them."""
    _poll(
        lambda: app.driver.count("onboarding-nest-rules") == 1,
        _SEND_BUDGET_S,
        f"{page} paints the nest's rules",
        diagnose=lambda: app.driver.diagnose("onboarding-nest-rules"),
    )
    heading = app.driver.get_text("onboarding-nest-rules")
    assert heading == S.nest_rules.joining_heading(nest=domain), f"{page}: {heading!r}"
    items = [
        app.driver.get_text("onboarding-nest-rules-item", index=i)
        for i in range(app.driver.count("onboarding-nest-rules-item"))
    ]
    assert any(reason in line for line in items), (
        f"{page}: a rule line carries the admin's reason {reason!r}: {items!r}"
    )
    assert app.is_visible("onboarding-terms-notice"), (
        f"{page}: the rules sit with the terms notice: "
        f"{app.driver.diagnose('onboarding-terms-notice')}"
    )


def _open_invite_request(app, nest_instance, token):
    """A fresh identity on a reset app asks this nest for an invite — the
    ``invite_request`` page, read before any sign-in."""
    import json

    from nacl.signing import SigningKey

    app.driver.reset()
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(bytes(SigningKey.generate()).hex())
    app.driver.wait_for("handle-input", timeout=_PAGE_BUDGET_S)
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_instance["url"], "joiner" + token]),
    )
    app.driver.wait_for("invite-request-submit-button", timeout=_PAGE_BUDGET_S)


@pytest.mark.feature("nest-rules")
def test_joining_shows_the_nests_rules_with_the_terms_notice(app, nest_instance, request):
    """A new identity's ``invite_request``, and an admin-admitted identity's
    ``handle_entry`` once its check concludes Continue signs in, both show the
    rules the admin published — read before sign-in, verified in shared Rust."""
    from urllib.parse import urlparse

    token = uuid.uuid4().hex[:10]
    reason = f"No joiner spam {token}."

    dial_url = _dial_url(app, nest_instance, request)

    # The admin publishes a rule through the shared editor.
    login_as_nest_admin(app, nest_instance, dial_url)
    app.admin.navigate_nest()
    app.rule_editor.wait_loaded()
    _write_keyword_rule(app, f"joinspam{token}", reason)
    app.rule_editor.save()
    _saved_sequence(app)
    with _admin_ws(nest_instance) as ws:
        domain = ws.call("fauna.nest.info", {})["domain"]

    # A new identity asks for an invite: invite_request shows the rules.
    _open_invite_request(app, nest_instance, token)
    _assert_the_joining_rules(app, domain, reason, "invite_request")

    # An identity the admin admitted signs in from handle_entry: the rules show
    # once the real handle check concludes Continue signs in, never before.
    secret_hex, localpart = _admit_fresh(nest_instance)
    ob = app.onboarding
    app.driver.reset()
    # The override routes the probe through the same address the admin dialled.
    app.driver.set_provider_base_urls({"nest": dial_url})
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=_PAGE_BUDGET_S)
    assert app.driver.is_absent("onboarding-nest-rules"), "no rules before the check"
    handle = f"{localpart}@{urlparse(nest_instance['url']).netloc}"
    app.driver.clear_and_type("handle-input", handle)
    app.driver.click("handle-check-button")
    _poll(
        lambda: app.driver.is_enabled("handle-entry-continue-button"),
        _SEND_BUDGET_S,
        "the admitted identity's check concludes AlreadyOnNest",
        diagnose=lambda: app.driver.get_text("handle-message-area"),
    )
    _assert_the_joining_rules(app, domain, reason, "handle_entry")


# ── Rules this device cannot apply, and the collapse reveal ─────────────────


def _joining_lines(app):
    return [
        app.driver.get_text("onboarding-nest-rules-item", index=i)
        for i in range(app.driver.count("onboarding-nest-rules-item"))
    ]


def _settings_rule_index(app, reason):
    """The settings rules surface's item carrying ``reason``, or ``None``."""
    if _rules_surface(app) is None:
        return None
    for i in range(app.driver.count("settings-nest-rules-item")):
        if reason in (app.driver.get_text("settings-nest-rules-item", index=i) or ""):
            return i
    return None


@pytest.mark.feature("nest-rules")
def test_a_rule_this_device_cannot_apply_is_listed_as_not_applied_at_joining_and_in_settings(
    app, nest_instance, request
):
    """``nest-rules.md`` decision 1 and § Acceptance at joining: an admin's
    ``prompt`` rule — admitted for every publisher, run by no app yet — is
    published like any other, and a member's device lists it as not applied on
    this device, in Settings and before sign-in on ``invite_request``; a rule it
    applies carries no such note."""
    token = uuid.uuid4().hex[:10]
    prompt_reason = f"No scams {token}."
    keyword_reason = f"No raffle spam {token}."
    note = S.rule_editor.prompt_not_applied
    dial_url = _dial_url(app, nest_instance, request)

    # The admin writes a prompt rule and an applied keywords rule in the editor.
    login_as_nest_admin(app, nest_instance, dial_url)
    app.admin.navigate_nest()
    app.rule_editor.wait_loaded()
    editor = app.rule_editor
    row = editor.add_rule()
    editor.pick(row, "kind", S.rule_editor.kind.prompt)
    _poll(
        lambda: editor.has_control(row, "rule-editor-item-prompt-input"),
        _PAGE_BUDGET_S,
        "a prompt rule paints its prompt input",
    )
    editor.type_input(row, "prompt", f"Is this post a scam? {token}")
    editor.type_input(row, "reason", prompt_reason)
    _poll(
        editor.save_enabled,
        _PAGE_BUDGET_S,
        "a filled prompt rule becomes saveable",
        diagnose=editor.status,
    )
    _write_keyword_rule(app, f"raffle{token}", keyword_reason)
    editor.save()
    _saved_sequence(app)

    # A member's Settings lists the prompt rule with the not-applied note.
    member = _actor(nest_instance)
    _login(app, nest_instance, member, request)
    _poll(
        lambda: _settings_rule_index(app, prompt_reason) is not None,
        _PAGE_BUDGET_S,
        "the member's rules surface lists the prompt rule",
        diagnose=lambda: app.driver.diagnose("settings-nest-rules-item"),
    )
    inert_at = _settings_rule_index(app, prompt_reason)
    scope = f"settings-nest-rules-item[{inert_at}]"
    assert app.driver.count("settings-nest-rules-inert-notice", scope=scope) == 1, (
        f"the prompt rule carries the not-applied note: "
        f"{app.driver.diagnose('settings-nest-rules-inert-notice')}"
    )
    assert app.driver.get_text("settings-nest-rules-inert-notice", scope=scope) == note
    applied_at = _settings_rule_index(app, keyword_reason)
    assert applied_at is not None, "the keywords rule is listed too"
    assert app.driver.count(
        "settings-nest-rules-inert-notice", scope=f"settings-nest-rules-item[{applied_at}]"
    ) == 0, "a rule this device applies carries no not-applied note"

    # Joining: invite_request lists the same rule with the same note under it.
    _open_invite_request(app, nest_instance, token)
    _poll(
        lambda: any(prompt_reason in line for line in _joining_lines(app)),
        _SEND_BUDGET_S,
        "invite_request lists the prompt rule",
        diagnose=lambda: _joining_lines(app),
    )
    lines = _joining_lines(app)
    prompt_line = next(line for line in lines if prompt_reason in line)
    assert note in prompt_line, f"the joining line carries the not-applied note: {prompt_line!r}"
    keyword_line = next((line for line in lines if keyword_reason in line), None)
    assert keyword_line is not None, f"the keywords rule is listed at joining: {lines!r}"
    assert note not in keyword_line, f"an applied rule carries no note: {keyword_line!r}"


@pytest.mark.feature("nest-rules")
def test_a_nest_collapse_rule_folds_the_post_behind_a_reveal(app, nest_instance, request):
    """``nest-rules.md`` § The nest content policy: a ``collapse`` keeps the
    engine's reveal affordance — the member's feed paints the nest placeholder
    with the reveal in place of the matching post's body, and revealing shows
    the body."""
    token = uuid.uuid4().hex[:10]
    term = f"bingo{token}"
    target_text = f"tonight's {term} night"
    reason = f"Bingo talk folded {token}."
    dial_url = _dial_url(app, nest_instance, request)
    author, member = _actor(nest_instance), _actor(nest_instance)

    _login(app, nest_instance, author, request)
    app.feed.navigate()
    app.feed.create_post(target_text)
    row = app.feed.wait_for_post_state_by_text(target_text, timeout=_FEED_BUDGET_S)
    assert row is not None, f"the target post must land; error={app.error_text()!r}"
    post_id = row["post_id"]

    # The admin writes a keywords rule whose verdict is collapse.
    login_as_nest_admin(app, nest_instance, dial_url)
    app.admin.navigate_nest()
    app.rule_editor.wait_loaded()
    _write_keyword_rule(app, term, reason)
    rule_row = app.rule_editor.rule_count() - 1
    app.rule_editor.pick(rule_row, "verdict", S.rule_editor.verdict.collapse)
    assert app.rule_editor.picked(rule_row, "verdict") == S.rule_editor.verdict.collapse
    app.rule_editor.save()
    _saved_sequence(app)

    # The member's feed folds the post behind the nest placeholder's reveal.
    _login(app, nest_instance, member, request)
    _general_feed(app)

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
    assert app.driver.get_attr("nest-blocked-notice", "verdict", scope=scope) == "collapse"
    domain = app.driver.get_text("nest-blocked-domain", scope=scope)
    assert app.driver.get_text("nest-blocked-notice", scope=scope) == (
        S.nest_rules.collapsed_notice(nest=domain)
    )
    assert app.driver.get_text("nest-blocked-reason", scope=scope) == reason
    assert app.driver.count("feed-post-text", scope=scope) == 0, (
        "a collapsed post renders no body before the reveal"
    )
    assert app.driver.count("nest-collapsed-reveal-button", scope=scope) == 1, (
        "a nest collapse offers the reveal"
    )

    # Revealing shows the body.
    app.driver.click("nest-collapsed-reveal-button", scope=scope)
    _poll(
        lambda: _card() is not None
        and app.driver.count("feed-post-text", scope=_card()) >= 1
        and app.driver.count("nest-blocked-notice", scope=_card()) == 0,
        _FEED_BUDGET_S,
        "the reveal shows the post's body",
        diagnose=lambda: (_card(), app.driver.diagnose("nest-blocked-notice")),
    )
    assert term in app.driver.get_text("feed-post-text", scope=_card())
