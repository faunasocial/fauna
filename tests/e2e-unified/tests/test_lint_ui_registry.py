"""tier_1: the two UI-id conformance directions `lint-ui-elements.py` never checked.

Pure in-process analysis of `scripts/lint-ui-registry.py` against synthetic trees
plus the real `ui.yaml`. No nest binary, no driver, no build.

The gap these guard. `scripts/lint-ui-elements.py` asks only "is every ui.yaml
element implemented by every app?" (spec → app). Two directions were unguarded,
and both carried live drift measured 2026-08-10:

  * internal  — ids named by a page/component list have no `elements:` registry
                row, though ui.yaml's header says the registry covers all ids.
                87 measured, closed to 0 the same day.
  * reverse   — 31 ids are rendered by an app but appear nowhere in ui.yaml, which
                ui.yaml rule A calls a deviation needing explicit user approval.

Both are the input-set question for a generated id-constant module: a generator
emitting from the registry cannot emit a constant for a page-list id the registry
never got.

Each test below is anchored on a distinct mutation of the script's semantics, so a
regression in any single report fails exactly one test.
"""

import importlib.util
import json
import os
import subprocess
import sys
import textwrap

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_SCRIPT = os.path.join(_REPO, "scripts", "lint-ui-registry.py")


def _load():
    spec = importlib.util.spec_from_file_location("lint_ui_registry", _SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture(scope="module")
def mod():
    return _load()


def _write_ui(root, body: str):
    ui_dir = root / "tests" / "e2e-unified"
    ui_dir.mkdir(parents=True, exist_ok=True)
    (ui_dir / "ui.yaml").write_text(textwrap.dedent(body), encoding="utf-8")


# ── the internal direction ────────────────────────────────────────────────────


def test_referenced_not_registered_flags_a_page_list_id_with_no_registry_row(mod, tmp_path):
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            elements:
              - feed-post
              - feed-ghost
        elements:
          feed-post:
            type: view
            description: "declared"
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert list(found["referenced-not-registered"]) == ["feed-ghost"]
    assert found["referenced-not-registered"]["feed-ghost"] == ["pages.feed.elements"]


def test_referenced_not_registered_reads_optional_and_platform_lists_too(mod, tmp_path):
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            optional_elements:
              - feed-optional-ghost
            platform_elements:
              web:
                - feed-web-ghost
        elements: {}
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert set(found["referenced-not-registered"]) == {"feed-web-ghost", "feed-optional-ghost"}
    assert found["referenced-not-registered"]["feed-web-ghost"] == [
        "pages.feed.platform_elements.web"
    ]


def test_registry_ids_are_never_mistaken_for_page_list_references(mod, tmp_path):
    """The bottom `elements:` mapping declares ids; it must not also count as a *use*."""
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            elements:
              - feed-post
        elements:
          feed-post:
            type: view
            description: "used by a page"
          feed-orphan:
            type: view
            description: "registered, but no page lists it"
        """,
    )
    found = mod.build_findings(("registered-not-referenced",), tmp_path)
    assert found["registered-not-referenced"] == ["feed-orphan"]


def test_a_page_level_components_list_counts_as_a_reference(mod, tmp_path):
    """A page's OWN `components:` list names a container id it uses (ui.yaml header:
    'List of component IDs used by this page') — measured 2026-08-10: 64 container ids
    (post-card-shaped: feed-selector, dm-message-bubble, subscription-tier-list, …) were
    invisible to both reports because this list shape went unwalked, hiding them from
    referenced-not-registered even though they lacked a registry row."""
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            components:
              - feed-selector
        elements: {}
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert "feed-selector" in found["referenced-not-registered"]
    assert found["referenced-not-registered"]["feed-selector"] == ["pages.feed.components"]


def test_navigation_tabs_counts_as_a_reference(mod, tmp_path):
    """navigation.tabs is a bare list of "{page}-tab" ids — a real use, just not
    inside an `elements:`-keyed list."""
    _write_ui(
        tmp_path,
        """
        navigation:
          tabs:
            - feed-tab
        elements: {}
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert "feed-tab" in found["referenced-not-registered"]


def test_navigation_gated_tabs_id_counts_as_a_reference(mod, tmp_path):
    """navigation.gated_tabs is [{id: ..., gate: ...}, ...] — the id is a nav entry
    exactly like a tabs list member, just dict-shaped for the extra gate metadata."""
    _write_ui(
        tmp_path,
        """
        navigation:
          gated_tabs:
            - id: admin-tab
              gate: am-i-admin
        elements: {}
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert "admin-tab" in found["referenced-not-registered"]


def test_navigation_sub_page_nav_rows_id_counts_as_a_reference(mod, tmp_path):
    """navigation.sub_page_nav_rows is {<shell>: {id: ..., key: ...}} — each shell's
    id is the indexed rail-row element, a nav entry like a gated tab's."""
    _write_ui(
        tmp_path,
        """
        navigation:
          sub_page_nav_rows:
            admin:
              id: admin-nav-row
              key: "the ui.yaml page key the row opens"
        elements: {}
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert "admin-nav-row" in found["referenced-not-registered"]


def test_a_children_list_counts_as_a_reference(mod, tmp_path):
    """`children:` names the child COMPONENT ids a container composes. Measured
    2026-08-11: unwalked, so 5 ids whose only reference was a `children:` list
    (post-card's interaction-bar / image-grid / post-tip-list / feed-post-actions-menu,
    dm-message-bubble's dm-message-actions-menu) were invisible to referenced-not-registered
    AND kept out of rendered-not-declared only by the raw-text fallback this pass removed.
    Third instance of the same shape after `components:` and `navigation.tabs`."""
    _write_ui(
        tmp_path,
        """
        components:
          post-card:
            description: "a card"
            children:
              - interaction-bar
        elements: {}
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert "interaction-bar" in found["referenced-not-registered"]
    assert found["referenced-not-registered"]["interaction-bar"] == [
        "components.post-card.children"
    ]


def test_an_errors_list_counts_as_a_reference(mod, tmp_path):
    """`errors: [error-message]` names the error element a page surfaces — a real use."""
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            errors: [error-message]
        elements: {}
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert "error-message" in found["referenced-not-registered"]


def test_every_id_list_key_in_the_real_ui_yaml_is_classified(mod):
    """The recurrence guard for this checker's one repeating defect.

    Three id-list key shapes (`components:`, `navigation.tabs`, `children:`) were each
    found unwalked *after* the lint shipped, and each hid real findings from both reports
    at once. So every list-of-strings key in the walked sections must be classified as
    either walked or explicitly not-ids. A new ui.yaml key fails here instead of going
    quietly unchecked — if this test reds, decide which set the key belongs in; do not
    delete the assertion.
    """
    doc, _raw = mod.load_ui(mod.REPO_ROOT)
    unknown = mod.unknown_id_list_keys(doc)
    assert unknown == {}, (
        "unclassified id-list key(s) in ui.yaml — add each to ID_LIST_KEYS (it names "
        f"element ids) or NON_ID_LIST_KEYS (it does not): {unknown}"
    )


def test_navigation_layouts_tabs_is_NOT_mistaken_for_navigation_tabs(mod, tmp_path):
    """navigation.layouts.mobile.tabs shares the bare key name "tabs" with
    navigation.tabs but lists PAGE names (e.g. "feed"), not element ids — a
    same-named-key false match here would flag every page name as an invented
    reference. Path-exact matching (child == "navigation.tabs") is what tells them
    apart; this pins that precision after a same-session near-miss where a first-cut
    fix matched on bare key name and flagged "feed"/"contacts"/"more" as referenced."""
    _write_ui(
        tmp_path,
        """
        navigation:
          layouts:
            mobile:
              tabs: [feed, contacts, more]
        elements: {}
        """,
    )
    found = mod.build_findings(("referenced-not-registered",), tmp_path)
    assert "feed" not in found["referenced-not-registered"]
    assert "contacts" not in found["referenced-not-registered"]
    assert "more" not in found["referenced-not-registered"]


# ── the reverse direction ─────────────────────────────────────────────────────


def test_rendered_not_declared_flags_an_id_absent_from_ui_yaml(mod, tmp_path):
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            elements:
              - feed-post
        elements:
          feed-post:
            type: view
            description: "declared"
        """,
    )
    web = tmp_path / "apps" / "fauna-web" / "src"
    web.mkdir(parents=True)
    (web / "Feed.svelte").write_text(
        '<div data-testid="feed-post"></div>\n<div data-testid="feed-invented"></div>\n',
        encoding="utf-8",
    )
    found = mod.build_findings(("rendered-not-declared",), tmp_path)
    assert list(found["rendered-not-declared"]["web"]) == ["feed-invented"]
    assert found["rendered-not-declared"]["web"]["feed-invented"].endswith("Feed.svelte:2")


def test_rendered_not_declared_skips_ignored_build_output_but_reads_untracked_source(
    mod, tmp_path
):
    """Ignored build output is not a render site. MSBuild's `obj/` keeps a copy of
    each page's XAML that outlives the page, so a deleted page's ids read as
    invented in the one checkout still holding it (2026-09-30). A new source file
    not yet committed IS still read — the walk keys on git's ignore rules, not
    on tracked-only."""
    _write_ui(tmp_path, "pages: {}\nelements: {}\n")
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    (tmp_path / ".gitignore").write_text("obj/\n", encoding="utf-8")
    web = tmp_path / "apps" / "fauna-web" / "src"
    (web / "obj").mkdir(parents=True)
    (web / "obj" / "Stale.svelte").write_text('<div data-testid="stale-copy"></div>', "utf-8")
    (web / "New.svelte").write_text('<div data-testid="new-invented"></div>', "utf-8")
    found = mod.build_findings(("rendered-not-declared",), tmp_path)
    assert list(found["rendered-not-declared"]["web"]) == ["new-invented"]


def test_a_page_list_id_still_missing_its_registry_row_is_not_reported_as_invented(
    mod, tmp_path
):
    """The two reports must not double-count: a page-list id IS declared."""
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            elements:
              - feed-unregistered
        elements: {}
        """,
    )
    web = tmp_path / "apps" / "fauna-web" / "src"
    web.mkdir(parents=True)
    (web / "Feed.svelte").write_text('<div data-testid="feed-unregistered"></div>', "utf-8")
    found = mod.build_findings(mod.REPORTS, tmp_path)
    assert found["rendered-not-declared"]["web"] == {}
    assert list(found["referenced-not-registered"]) == ["feed-unregistered"]


def test_a_prose_mention_is_NOT_a_declaration(mod, tmp_path):
    """The loophole this report shipped with, closed 2026-08-11.

    `rendered-not-declared` used to fall back to a whole-file regex over ui.yaml's raw
    text, so an id named only inside a **comment** counted as declared and never surfaced.
    That hid three live rule-A findings on the real tree — `nostr-settings-link` and
    `atproto-settings-link` (described in prose notes as built, never declared) and
    `role-badge`, whose only mention is a comment recording it as REMOVED while apple
    still renders it (`ui.yaml:8386`). A comment describing an id is not a declaration.
    """
    _write_ui(
        tmp_path,
        """
        pages:
          settings:
            elements:
              - account-settings-link
        # nostr reachable only via SettingsView's nostr-settings-link (a prose note)
        elements:
          account-settings-link:
            type: button
            description: "declared"
        """,
    )
    web = tmp_path / "apps" / "fauna-web" / "src"
    web.mkdir(parents=True)
    (web / "Settings.svelte").write_text(
        '<a data-testid="account-settings-link"></a>\n'
        '<a data-testid="nostr-settings-link"></a>\n',
        encoding="utf-8",
    )
    found = mod.build_findings(("rendered-not-declared",), tmp_path)
    assert list(found["rendered-not-declared"]["web"]) == ["nostr-settings-link"]


def test_a_page_or_component_key_IS_a_declaration(mod, tmp_path):
    """The other half of replacing the raw-text fallback: a top-level `pages:` /
    `components:` / `onboarding:` key is the container's own id, and apps legitimately
    render it on the block itself (`web-settings`, `admin-bridges-rotate-confirm` on the
    real tree). Dropping the raw-text fallback must not turn those into false findings."""
    _write_ui(
        tmp_path,
        """
        pages:
          web-settings:
            elements: []
        components:
          post-card:
            description: "a card"
        onboarding:
          handle-step:
            elements: []
        elements: {}
        """,
    )
    web = tmp_path / "apps" / "fauna-web" / "src"
    web.mkdir(parents=True)
    (web / "P.svelte").write_text(
        '<div data-testid="web-settings"></div>\n'
        '<div data-testid="post-card"></div>\n'
        '<div data-testid="handle-step"></div>\n',
        encoding="utf-8",
    )
    found = mod.build_findings(("rendered-not-declared",), tmp_path)
    assert found["rendered-not-declared"]["web"] == {}


def test_harness_probe_ids_are_declared_but_not_registry_elements(mod, tmp_path):
    """ui.yaml's `harness_probe_ids:` list exempts harness self-test fixtures (ratified
    2026-08-11), so the reverse gate can go strict without a probe being read as product
    surface every app owes. It must exempt ONLY the listed ids — not act as a blanket
    escape hatch — and must not make them registry elements."""
    _write_ui(
        tmp_path,
        """
        pages: {}
        harness_probe_ids:
          - smoke-target
        elements: {}
        """,
    )
    linux = tmp_path / "apps" / "fauna-linux" / "src"
    linux.mkdir(parents=True)
    (linux / "testid.rs").write_text(
        'set_test_id(&w, "smoke-target");\n'
        'set_test_id(&w, "not-a-probe");\n',
        encoding="utf-8",
    )
    found = mod.build_findings(mod.REPORTS, tmp_path)
    assert list(found["rendered-not-declared"]["linux"]) == ["not-a-probe"]
    # exempt from the reverse report, but NOT smuggled into the registry
    assert found["registered-not-referenced"] == []
    assert found["referenced-not-registered"] == {}


def test_each_app_is_anchored_on_its_own_id_setting_syntax(mod, tmp_path):
    """A bare kebab string that is not an id-set call must not be flagged."""
    _write_ui(tmp_path, "pages: {}\nelements: {}\n")
    tui = tmp_path / "apps" / "fauna-tui" / "src"
    tui.mkdir(parents=True)
    (tui / "page.rs").write_text(
        'let term = "xterm-kitty";\n'
        'els.push(Element::label("tui-invented", ""));\n',
        encoding="utf-8",
    )
    found = mod.build_findings(("rendered-not-declared",), tmp_path)
    assert list(found["rendered-not-declared"]["tui"]) == ["tui-invented"]


def test_every_app_has_an_anchor_and_they_cover_the_seven_areas(mod):
    """apple covers macos+ios, so six entries span all 7 app areas."""
    assert set(mod.APPS) == {"web", "windows", "android", "apple", "linux", "tui"}
    for app, spec in mod.APPS.items():
        assert spec["patterns"], app
        assert (os.path.join(_REPO, spec["dir"])), app
        assert os.path.isdir(os.path.join(_REPO, spec["dir"])), f"{app}: {spec['dir']} missing"


# ── the gate contract ─────────────────────────────────────────────────────────


def test_advisory_by_default_and_strict_gates(tmp_path):
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            elements:
              - feed-ghost
        elements: {}
        """,
    )
    base = [sys.executable, _SCRIPT, "--root", str(tmp_path)]
    assert subprocess.run(base, capture_output=True).returncode == 0
    assert subprocess.run(base + ["--strict"], capture_output=True).returncode == 1


def test_strict_exits_zero_on_a_clean_tree(tmp_path):
    _write_ui(
        tmp_path,
        """
        pages:
          feed:
            elements:
              - feed-post
        elements:
          feed-post:
            type: view
            description: "declared and used"
        """,
    )
    done = subprocess.run(
        [sys.executable, _SCRIPT, "--root", str(tmp_path), "--strict"],
        capture_output=True,
    )
    assert done.returncode == 0, done.stdout.decode() + done.stderr.decode()


def test_json_output_carries_all_three_reports(tmp_path):
    _write_ui(tmp_path, "pages: {}\nelements: {}\n")
    done = subprocess.run(
        [sys.executable, _SCRIPT, "--root", str(tmp_path), "--json"],
        capture_output=True,
        check=True,
    )
    payload = json.loads(done.stdout.decode())
    assert set(payload) == set(
        ("referenced-not-registered", "registered-not-referenced", "rendered-not-declared")
    )


# ── the real tree: the measured backlog is non-empty and must shrink, not grow ──


def test_real_ui_yaml_backlog_is_bounded(mod):
    """Down-only ceilings on the drift measured 2026-08-10.

    These are NOT targets — they are the grandfathered baseline. Lower them when a
    triage pass lands; never raise one to make a new violation pass. Raising a
    ceiling is the signal that an undeclared id shipped.

    referenced-not-registered closed to 0 the same day (bucket 1): every
    id a page/component list named now has a registry row. That pass also fixed a
    collector gap in `collect_referenced` — a page's own `components:` list and
    `navigation.tabs`/`gated_tabs` were never walked, so 64 additional container ids
    (post-card-shaped: feed-selector, dm-message-bubble, subscription-tier-list, …)
    were invisible to BOTH reports; fixing the collector surfaced them as
    referenced-not-registered (closed the same pass) and dropped
    registered-not-referenced's false-positive share, so its ceiling moves 45 → 28
    even though bucket 2's actual triage (retire vs missing-list-entry, per id) has
    not started.

    ⚠ rendered-not-declared's ceiling moved UP once, 31 → 35, on 2026-08-11 — the one
    sanctioned reason: **the checker got more honest, the drift did not grow.** Removing
    the raw-text fallback (a prose mention no longer counts as a declaration, see
    `test_a_prose_mention_is_NOT_a_declaration`) surfaced four findings that were always
    real and always shipping: `nostr-settings-link` (apple + windows),
    `atproto-settings-link` (apple), and `role-badge` (apple, recorded in a ui.yaml
    comment as REMOVED while apple still renders it). The same pass walked `children:`,
    which added 5 to referenced-not-registered — all 5 registered in that commit, so this
    arm stayed at 0. From here the ceiling is down-only again: any further increase means
    an undeclared id shipped, NOT that the lint improved.

    Then 35 → 28 the same day: the 7 harness self-test probes moved to ui.yaml's declared
    `harness_probe_ids:` exemption (ratified 2026-08-11), so they are no longer reported.
    The 28 that remain are all real product ids whose declarations the user approved on
    2026-08-11 but which are not yet written into ui.yaml.

    28 → 35 by 2026-08-15 (the append note): NOT drift — five tui tests in
    automation.rs and two in app.rs had invented plausible-sounding but fictional
    element ids (post-body, history-item, divergence-banner, step-row, step-label,
    compose-submit, bridge-setting-toggle) instead of the real ids the app already
    declares. Fixed by pointing the fixtures at the real ids (feed-post-text,
    restore-history-item, restore-divergence-banner, provisioning-step-row,
    provisioning-step-label, post-submit-button, an empty id matching production).

    35 → 1 the same day (the declare pass): all 26 user-approved ids from the
    2026-08-11 table are now declared (registry row + a page/component/platform_elements
    reference each — none left registry-only, so registered-not-referenced's ceiling did
    not move). The one remaining finding, `role-badge` (apple), is deliberately NOT row
    23's — it is a separately-tracked rename to `device-folder-role-badge`.

    registered-not-referenced 28 → 0 (the close, 2026-08-15): the per-id
    archaeology on the 28 orphans. 15 were genuinely retired with zero current
    implementation (get-started-button, handle-avail, handle-register-btn,
    wizard-back-btn/wizard-next-btn — all superseded by wizard-back-button/
    wizard-next-button, settings-update-button, settings-inbox-mode-combo —
    superseded by the Privacy sub-page's inbox-mode-* radios, media-file-list,
    event-item, nest-{dns,server,registrar}-back-button — all three superseded by
    the unified provisioning-back-button, and devices-tab/status-tab/sync-tab — all
    three casualties of the 2026-06-28 sync/folder UI unification with zero
    current implementation) — deleted. The other 13 were real, shipping ids simply
    missing a list entry (event-detail-description/-location/-time/-rsvp-going/
    -rsvp-interested/-rsvp-decline + attendee-id/attendee-status → events.event_detail's
    elements; search-tab → search's optional_elements; settings-view/
    settings-nest-url-field → status's elements; settings-handle-label → settings'
    optional_elements; main-tab-view → global.platform_elements.ios) — declared.
    Now gates `--strict` on the cheap merge tier (both merge scripts) alongside
    referenced-not-registered.

    1 → 0 (2026-08-25, row 378): the windows trust facet grew two container ids —
    `nest-trust-backup-list` / `nest-trust-generation-list` (NestsPanel.xaml:192,247)
    — without ever declaring them in ui.yaml, alongside their long-declared sibling
    `nest-trust-grant-list`. Declared both (`components.nests-item.elements` plus a
    registry entry each, mirroring `nest-trust-grant-list`'s shape) rather than
    raising the ceiling — the docstring's own rule.

    1 → 0 again (2026-09-11): `row-470-indexed-state`
    (`apps/fauna-linux/src/automation/agent.rs:599`) is a HARNESS PROBE, not product
    surface — it renders only from the `#[cfg(test)] mod tests` block at :502, the same
    one that owns the already-exempted `row-40-empty-entry`, so it qualifies under
    ui.yaml's `harness_probe_ids:` rule ("renders solely from test/automation
    scaffolding — never from a code path a user's app can reach"). Listed there, NOT in
    the `elements:` registry: a registry row would make all 7 apps owe a probe. It went
    undetected for its whole life because nothing runs this directory — the darkness
    row 88 exists to end.

    3 → 0 (2026-09-26, the win tier_1 gate): `options-attr-dropdown` /
    `options-attr-combo` / `options-attr-label` (`apps/fauna-linux/src/automation/agent.rs`
    ~:1083-1090) are the `options` attr-read probe's three widgets — the same
    `#[cfg(test)] mod tests` block as the two probes above, so the same
    `harness_probe_ids:` listing, for the same reason. Caught by the gate a day after
    they landed, which is the darkness ending.
    """
    found = mod.build_findings(mod.REPORTS)
    assert len(found["referenced-not-registered"]) <= 0
    assert len(found["registered-not-referenced"]) <= 0
    rendered = sum(len(v) for v in found["rendered-not-declared"].values())
    assert rendered <= 0


def test_row_23_declared_ids_no_longer_flagged(mod):
    """Regression guard for the declare pass (2026-08-15): these ids —
    the memo's tui worked example plus the rest of the 26 approved bookkeeping
    ids — must stay declared. `role-badge` is deliberately excluded: it is
    a separately-tracked rename, not the declare."""
    found = mod.build_findings(("rendered-not-declared",))
    for app, ids in {
        "tui": [
            "admin-bridges-approved-section",
            "admin-bridges-pending-section",
            "conversations-view",
            "mail-rotate-keys-exclude-item",
        ],
        "web": ["admin-dns-empty", "admin-factory-reset-cancel-button", "nests-section", "sign-out-cancel-button"],
        "windows": ["card-detail-back", "nostr-signing-mode"],
        "android": ["family-guardian-section", "family-supervised-section"],
        "apple": [
            "atproto-settings-link",
            "devices-settings-link",
            "folders-settings-link",
            "launch-error-title",
            "launch-status",
            "linked-nests-link",
            "mail-settings-link",
            "more-back-button",
            "more-tab",
            "more-view",
            "nostr-settings-link",
            "subscription-settings-link",
            "task-delegation-link",
            "web-settings-link",
        ],
    }.items():
        flagged = found["rendered-not-declared"][app]
        for element_id in ids:
            assert element_id not in flagged, f"{element_id} regressed back to undeclared on {app}"
