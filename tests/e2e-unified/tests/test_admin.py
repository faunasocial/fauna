import re
import secrets
import sqlite3
import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register, ensure_tier, register_user, set_user_tier
from helpers import budgets
from helpers.app_surface import app_name, skip_environment, skip_unbuilt
from helpers.rpc_hold import refuse_rpc, release_rpc_hold
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("admin-tiers")
def test_settings_tier_definitions(admin_app):
    """admin-settings shows the tier *definitions* (admin.md § 3 Settings).

    The nest seeds free/personal/community tiers; the settings page renders them
    as indexed `admin-settings-tier-item` rows under the
    `admin-settings-tiers-section` anchor (driven by `fauna.admin.tiers.list`).
    """
    admin_app.admin.navigate_settings()
    assert admin_app.admin.tiers_section_visible(), (
        f"tiers section missing. error: {admin_app.error_text()!r}"
    )
    # The tier list is fetched async on the admin-status check; poll for rows.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.admin.tier_definition_count() < 1:
        time.sleep(0.3)
    assert admin_app.admin.tier_definition_count() >= 1, (
        f"no tier definitions rendered. error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-tiers")
def test_settings_tier_edit(admin_app):
    """admin can edit a tier definition's caps in place (admin.md § 3 Settings).

    Each indexed `admin-settings-tier-item` row carries editable raw-i64 cap
    inputs (`admin-settings-tier-cap-*`) + an `admin-settings-tier-save-button`.
    Saving fires `fauna.admin.tiers.update`; the page then refetches `tiers.list`,
    so reading the input back after save proves the new value persisted in the
    nest (not merely echoed in the widget).
    """
    admin_app.admin.navigate_settings()
    # Tier definitions load async on the admin-status check; poll for rows.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.admin.tier_definition_count() < 1:
        time.sleep(0.3)
    assert admin_app.admin.tier_definition_count() >= 1, (
        f"no tier definitions rendered. error: {admin_app.error_text()!r}"
    )

    # Pick a new inbox-bytes cap distinct from whatever the row currently holds.
    current = admin_app.admin.tier_cap_value("inbox", index=0)
    new_value = "424242" if current.strip() != "424242" else "131313"
    admin_app.admin.edit_tier_cap("inbox", new_value, index=0)
    admin_app.admin.save_tier(index=0)

    # The save refetches tiers.list and re-renders the row from persisted state.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if admin_app.admin.tier_cap_value("inbox", index=0).strip() == new_value:
            break
        time.sleep(0.3)
    assert admin_app.admin.tier_cap_value("inbox", index=0).strip() == new_value, (
        "tier inbox cap did not persist after save+refetch "
        f"(got {admin_app.admin.tier_cap_value('inbox', index=0)!r}). "
        f"error: {admin_app.error_text()!r}"
    )


# Apps that paint `admin-settings-tier-delete-button` (admin.md § 3 — *Removing
# a tier*): tui leads, the other six follow in the batched trickle-down.
_TIER_DELETE_BUILT_APPS = {"tui", "linux", "web", "android"}


def _wait_for_tier_rows(admin_app, at_least: int = 1) -> None:
    """Tier definitions load async on the admin-status check: poll for rows."""
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.admin.tier_definition_count() < at_least:
        time.sleep(0.3)
    assert admin_app.admin.tier_definition_count() >= at_least, (
        f"tier definitions did not load. error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-tiers")
def test_settings_tier_define_new(admin_app):
    """An admin defines a NEW tier from the Tiers page (admin.md § 3 — *Defining
    a new tier*): name + the five raw-i64 caps in the add form, then
    `admin-settings-tier-add-button` fires `fauna.admin.tiers.create`.

    The page refetches `tiers.list`, so the new tier showing up as an ordinary
    row — name and every cap read back — proves it persisted in the nest, not
    merely that the form echoed the typed values.
    """
    admin_app.admin.navigate_settings()
    _wait_for_tier_rows(admin_app)
    assert admin_app.admin.tier_add_section_visible(), (
        f"add-a-tier form missing. error: {admin_app.error_text()!r}"
    )
    before = admin_app.admin.tier_definition_count()

    caps = {
        "inbox": "111000", "storage": "222000", "devices": "64",
        "blob-size": "333000", "feeds": "1000",
    }
    # A per-run name: the nest outlives one app of a cross-app sweep, so a fixed
    # name would meet the previous app's tier (`fauna.admin.conflict`).
    new_name = f"e2e-harness-{secrets.token_hex(3)}"
    admin_app.admin.edit_new_tier_name(new_name)
    for cap, value in caps.items():
        admin_app.admin.edit_new_tier_cap(cap, value)
    admin_app.admin.add_tier()

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.admin.tier_definition_count() <= before:
        time.sleep(0.3)
    assert admin_app.admin.tier_definition_count() == before + 1, (
        f"the new tier did not appear after create+refetch. "
        f"rows: {admin_app.admin.tier_names()!r}. error: {admin_app.error_text()!r}"
    )
    index = admin_app.admin.tier_names().index(new_name)
    for cap, value in caps.items():
        assert admin_app.admin.tier_cap_value(cap, index=index).strip() == value, (
            f"cap {cap!r} of the new tier did not persist"
        )


@pytest.mark.feature("admin-tiers")
def test_settings_tier_define_refuses_empty_and_duplicate_names(admin_app):
    """An empty name is refused locally and a taken name by the nest
    (`fauna.admin.conflict`); both surface on `error-message` and neither adds a
    row — a refusal the admin can read, never a silent no-op."""
    admin_app.admin.navigate_settings()
    _wait_for_tier_rows(admin_app)
    before = admin_app.admin.tier_definition_count()
    existing = admin_app.admin.tier_name(index=0)

    for cap, value in {"inbox": "1", "storage": "1", "devices": "1",
                       "blob-size": "1", "feeds": "1"}.items():
        admin_app.admin.edit_new_tier_cap(cap, value)

    admin_app.admin.edit_new_tier_name("")
    admin_app.admin.add_tier()
    empty_refusal = admin_app.error_text().strip()
    assert empty_refusal, "an empty tier name was refused silently"

    admin_app.admin.edit_new_tier_name(existing)
    admin_app.admin.add_tier()
    # The nest's refusal replaces the local one: wait for the text to change.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.error_text().strip() == empty_refusal:
        time.sleep(0.3)
    assert admin_app.error_text().strip() != empty_refusal, (
        "a taken tier name was refused silently (or with the empty-name text)"
    )
    assert admin_app.admin.tier_definition_count() == before


@pytest.mark.feature("admin-tiers")
def test_settings_tier_delete(admin_app, nest_instance):
    """An admin deletes a tier from the Tiers page (admin.md § 3 — *Removing a
    tier*): `admin-settings-tier-delete-button` fires `fauna.admin.tiers.delete`,
    with no confirm dialog.

    Deleting a tier a member is on is the nest's refusal (`fauna.admin.conflict`)
    and must read on `error-message` with the row still listed; deleting a tier
    nobody is on drops the row after the refetch. The two tiers and the member
    are precondition setup through the nest's admin API (defining a tier from
    the page is `test_settings_tier_define_new`); both deletes go through the UI.
    """
    if app_name(admin_app.driver) not in _TIER_DELETE_BUILT_APPS:
        skip_unbuilt(
            admin_app.driver,
            surface="admin-settings-tier-delete-button",
            detail="deleting a tier from the Tiers page is not built on this app yet",
            tracked="admin.md § 3. Settings → Removing a tier",
        )
    port, url = nest_instance["port"], nest_instance["url"]
    sk = nest_instance["admin"]["signing_key"]
    # Per-run names: the nest outlives one app of a cross-app sweep.
    suffix = secrets.token_hex(3)
    in_use, unused = f"e2e-in-use-{suffix}", f"e2e-unused-{suffix}"
    ensure_tier(port, admin_signing_key=sk, name=in_use, base_url=url)
    ensure_tier(port, admin_signing_key=sk, name=unused, base_url=url)
    member = create_actor_and_register(port, admin_signing_key=sk, base_url=url)
    set_user_tier(port, member["actor_id_hex"], in_use, admin_signing_key=sk, base_url=url)

    admin_app.admin.navigate_settings()
    wait_until(
        lambda: {in_use, unused} <= set(admin_app.admin.tier_names()),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"rows: {admin_app.admin.tier_names()!r}; "
        f"error: {admin_app.error_text()!r}",
    )

    # A tier a member is on: refused, the refusal readable, the row kept.
    admin_app.admin.delete_tier(index=admin_app.admin.tier_names().index(in_use))
    refusal = wait_until(
        lambda: admin_app.error_text().strip(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"rows: {admin_app.admin.tier_names()!r}",
    )
    assert S.admin.settings_page.delete_tier_error_in_use in refusal, (
        f"the in-use refusal does not say why: {refusal!r}"
    )
    assert in_use in admin_app.admin.tier_names(), "a refused delete removed the row"

    # A tier nobody is on: the row leaves the list after the refetch.
    admin_app.admin.delete_tier(index=admin_app.admin.tier_names().index(unused))
    wait_until(
        lambda: unused not in admin_app.admin.tier_names(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"rows: {admin_app.admin.tier_names()!r}; "
        f"error: {admin_app.error_text()!r}",
    )
    assert in_use in admin_app.admin.tier_names()


@pytest.mark.feature("admin-dashboard")
def test_admin_dashboard_loads(admin_app):
    """Admin dashboard shows stat cards after claiming admin."""
    admin_app.admin.navigate_dashboard()
    assert admin_app.admin.is_dashboard_visible()


@pytest.mark.feature("admin-dashboard")
def test_admin_dashboard_user_count(admin_app):
    """Dashboard shows at least 1 user (the admin)."""
    admin_app.admin.navigate_dashboard()
    value = admin_app.admin.dashboard_card_value("Users")
    assert int(value) >= 1


@pytest.mark.feature("admin-dashboard")
def test_admin_dashboard_version(admin_app):
    """Dashboard shows a non-empty version string."""
    admin_app.admin.navigate_dashboard()
    value = admin_app.admin.dashboard_card_value("Version")
    assert len(value) > 0


_SIZE = re.compile(r"([\d.,]+)\s*(B|KB|MB|GB|TB)\b")
_UNIT = {"B": 1, "KB": 1024, "MB": 1024**2, "GB": 1024**3, "TB": 1024**4}


def _storage_card(admin) -> tuple[str, str]:
    """The (label, value) of the card that shows the members' stored files.

    On the shared card set (`admin.md` § 1. Dashboard → *The card set*) that is
    the *File Storage* part beside Total Storage, which is inbox + files. An app
    still on its own cards shows the files counter alone under its first
    storage label ("Total Storage Used", "Total Storage", …), so that label is
    the fallback."""
    count = admin.driver.count("admin-stat-card-label")
    cards = [
        (
            admin.driver.get_text("admin-stat-card-label", index=i) or "",
            admin.driver.get_text("admin-stat-card-value", index=i) or "",
        )
        for i in range(count)
    ]
    for label, value in cards:
        if label == S.admin.dashboard.file_storage:
            return label, value
    for label, value in cards:
        if "storage" in label.lower():
            return label, value
    return "", ""


def _dashboard_cards(admin) -> list[tuple[str, str]]:
    """Every painted `admin-stat-card` as (label, value), in order."""
    count = admin.driver.count("admin-stat-card-label")
    return [
        (
            admin.driver.get_text("admin-stat-card-label", index=i) or "",
            admin.driver.get_text("admin-stat-card-value", index=i) or "",
        )
        for i in range(count)
    ]


def _size_bytes(value: str) -> float:
    m = _SIZE.search(value)
    assert m, f"{value!r} should read as a size"
    return float(m.group(1).replace(",", "")) * _UNIT[m.group(2)]


@pytest.mark.feature("admin-dashboard")
def test_admin_dashboard_shows_the_one_card_set(admin_app, nest_instance):
    """admin.md § 1. Dashboard → *The card set*: the dashboard shows the one
    ruled set, in order — members, suspended members, a card per tier, total
    storage with its inbox and files parts, live connections, mail, secure
    connections, the domain, the version and registration — and each card
    carries what the nest itself answers on the read behind it.

    A member is admitted, moved to a second tier and suspended first, so the
    suspended card and the per-tier cards carry non-trivial values. The nest's
    own answers are read over the admin control plane after the dashboard has
    loaded, and the painted set must match them card for card."""
    sk = SigningKey.generate()
    actor_id = bytes(sk.verify_key)
    register_user(
        nest_instance["port"],
        actor_id.hex(),
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
        handle=f"dashcard{secrets.token_hex(3)}",
        label="dashboard-card-member",
    )
    admin = nest_instance["admin"]
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        client.call(
            "fauna.admin.users.update",
            {"actor_id": actor_id, "tier": "personal", "label": "dashboard-card-member"},
        )
        client.call(
            "fauna.admin.users.suspend",
            {"actor_id": actor_id, "category": "other", "reason": "dashboard card set e2e"},
        )

        admin_app.admin.navigate_dashboard()
        cards = wait_until(
            lambda: (c := _dashboard_cards(admin_app.admin))
            and any(label == S.admin.dashboard.registration for label, _ in c)
            and c,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the card set did not finish loading: "
            f"{_dashboard_cards(admin_app.admin)!r}; error: {admin_app.error_text()!r}",
        )

        stats = client.call("fauna.admin.stats", {})
        status = client.call("fauna.admin.status", {})
        setup = client.call("fauna.setup.status", {})
        info = client.call("fauna.nest.info", {})
        try:
            mail_state = client.call("fauna.bridges.mail_health", {})["state"]
        except Exception:  # noqa: BLE001 — a failed read drops its own card
            mail_state = None

    tiers = [(str(name), int(count)) for name, count in stats["users_by_tier"]]
    assert stats["suspended_users"] >= 1 and len(tiers) >= 2, (
        f"the seed should leave a suspended member and two tiers: {stats!r}"
    )
    d = S.admin.dashboard
    expected_labels = (
        [S.admin.view.registered_users, d.suspended]
        + [name for name, _ in tiers]
        + [d.total_storage, d.inbox_storage, d.file_storage, d.connections]
        + ([d.mail] if mail_state is not None else [])
        + [d.secure_connections, d.nest_domain, d.version, d.registration]
    )
    assert [label for label, _ in cards] == expected_labels, (
        f"the painted card set {cards!r} is not the ruled set {expected_labels!r}"
    )
    value = dict(cards)
    assert value[S.admin.view.registered_users] == str(stats["total_users"])
    assert value[d.suspended] == str(stats["suspended_users"])
    for name, count in tiers:
        assert value[name] == str(count), f"tier {name!r}: {value[name]!r} vs {count}"
    inbox, files = stats["total_inbox_bytes"], stats["total_storage_bytes"]
    for label, want in (
        (d.total_storage, inbox + files),
        (d.inbox_storage, inbox),
        (d.file_storage, files),
    ):
        shown = _size_bytes(value[label])
        assert abs(shown - want) <= max(0.05 * want, 1), (
            f"the {label!r} card reads {value[label]!r}; the nest holds {want} B"
        )
    assert int(value[d.connections]) >= 1, "the app's own connection is live"
    if mail_state is not None:
        assert value[d.mail] == getattr(
            S.admin.mail_page,
            f"health_state_{mail_state}",
            S.admin.mail_page.health_state_unknown,
        ), f"mail card {value[d.mail]!r} for state {mail_state!r}"
    assert value[d.secure_connections] == (
        d.secure_on if setup["tls_active"] else d.secure_off
    )
    assert value[d.nest_domain] == info["domain"]
    assert value[d.version] == status["version"]
    assert value[d.registration] == getattr(
        S.admin.users_page, f"registration_mode_{setup['registration_mode']}"
    ), "registration reads as its own mode, never an open/closed pair"


@pytest.mark.parametrize(
    "kind,dropped",
    [
        ("fauna.bridges.mail_health", lambda d: [d.mail]),
        ("fauna.setup.status", lambda d: [d.secure_connections, d.registration]),
    ],
    ids=["mail-health", "setup-status"],
)
@pytest.mark.feature("admin-dashboard")
def test_a_failed_dashboard_read_drops_only_its_own_cards(
    admin_app, nest_instance, kind, dropped
):
    """admin.md § 1. Dashboard: one of the five reads behind the cards is turned
    away; the page shows the error, the cards that read feeds are gone and every
    other card is still there. The nest refuses the kind (`rpc-hold` refuse
    hook) for exactly this window and answers it again afterwards."""
    d = S.admin.dashboard
    gone = dropped(d)
    port = nest_instance["port"]

    admin_app.admin.navigate_dashboard()
    full = wait_until(
        lambda: (c := _dashboard_cards(admin_app.admin))
        and any(label == d.registration for label, _ in c)
        and [label for label, _ in c],
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the baseline dashboard never loaded: "
        f"{_dashboard_cards(admin_app.admin)!r}; error: {admin_app.error_text()!r}",
    )
    assert all(label in full for label in gone), (full, gone)

    refuse_rpc(port, kind)
    try:
        # The dashboard re-reads on every entry. Leave the shell and come back
        # the way a person does (entering lands on the dashboard), so no other
        # admin page's load can answer after the dashboard's and clear its error.
        admin_app.admin.leave_admin()
        admin_app.driver.wait_for("admin-tab", timeout=budgets.RPC_ROUNDTRIP_S)
        admin_app.admin.enter_by_click()
        wait_until(
            lambda: admin_app.error_text() or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: "the refused read never surfaced an error: "
            f"cards={_dashboard_cards(admin_app.admin)!r}",
        )
        kept = [label for label in full if label not in gone]
        shown = wait_until(
            lambda: (c := [label for label, _ in _dashboard_cards(admin_app.admin)])
            and kept[0] in c
            and c,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the other cards never came back: "
            f"{_dashboard_cards(admin_app.admin)!r}",
        )
        assert [label for label in shown if label in gone] == [], (
            f"the refused read's cards must be dropped: {shown!r}"
        )
        assert [label for label in shown] == kept, (
            f"every other card stays, in order: shown {shown!r}, expected {kept!r}"
        )
    finally:
        release_rpc_hold(port, kind)


@pytest.mark.feature("admin-dashboard")
def test_admin_dashboard_shows_the_storage_the_nest_holds(admin_app, nest_instance):
    """admin.md § 1. Dashboard: the dashboard shows how much storage the nest
    is using. Seven MiB is planted on the admin's own storage counter, so the
    card has a number it can only show by reading the nest: re-entering the
    dashboard (it re-reads on entry, no client cache), the storage card reads
    a size within a few percent of what the nest now holds. The counter is put
    back afterwards so no later test meets a full quota."""
    db_path = nest_instance.get("db_path")
    if not db_path:
        skip_environment("the storage counter is planted in the nest's database, "
                         "which this nest mode does not expose")
    admin_id = bytes(nest_instance["admin"]["signing_key"].verify_key)
    planted = 7 * 1024**2
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        before = conn.execute(
            "SELECT COALESCE(SUM(storage_bytes_used), 0) FROM users"
        ).fetchone()[0]
        (own,) = conn.execute(
            "SELECT storage_bytes_used FROM users WHERE actor_id = ?", (admin_id,)
        ).fetchone()
        conn.execute(
            "UPDATE users SET storage_bytes_used = ? WHERE actor_id = ?",
            (own + planted, admin_id),
        )
        conn.commit()
        try:
            # Leave and re-enter: the fixture may already stand on the dashboard,
            # and an app that routes to the page it is on does not re-read.
            admin_app.admin.navigate_users()
            admin_app.admin.navigate_dashboard()
            label, value = wait_until(
                lambda: (card := _storage_card(admin_app.admin))[1] and card,
                budgets.RPC_ROUNDTRIP_S,
                diagnose=lambda: f"no storage card: "
                f"{admin_app.driver.diagnose('admin-stat-card-label')}",
            )
            m = _SIZE.search(value)
            assert m, f"the {label!r} card should read as a size, got {value!r}"
            shown = float(m.group(1).replace(",", "")) * _UNIT[m.group(2)]
            expected = before + planted
            assert abs(shown - expected) <= 0.05 * expected, (
                f"the {label!r} card reads {value!r} (~{shown:.0f} B); the nest "
                f"holds ~{expected} B in members' storage"
            )
        finally:
            conn.execute(
                "UPDATE users SET storage_bytes_used = ? WHERE actor_id = ?",
                (own, admin_id),
            )
            conn.commit()
    finally:
        conn.close()


@pytest.mark.feature("admin-users")
def test_admin_user_list(admin_app, nest_instance):
    """Admin user list shows registered users."""
    admin_app.admin.navigate_users()
    assert admin_app.admin.user_count() >= 1


@pytest.mark.feature("admin-users")
def test_admin_invite_codes(admin_app):
    """Admin can mint invite codes from the consolidated admin-users Invite section."""
    admin_app.admin.navigate_invite_codes()
    initial = admin_app.admin.invite_code_count()
    code = admin_app.admin.create_invite_code()
    assert len(code) > 0
    assert admin_app.admin.invite_code_count() == initial + 1
    # The freshly minted token is surfaced copyable (admin.md § 2 — mint-on-empty
    # returns AdminInviteCodeCreateReply.code, shown via the copy button).
    assert admin_app.admin.copy_button_visible()
    admin_app.admin.copy_minted_code()  # tree-safe: must not raise
