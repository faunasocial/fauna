"""Web paywall — the per-set CLIENT-UI control (`folder-paywall-tier-select`),
linux-lead, windows-lifted (docs/goal/ui/folders.md § Web paywall /
monetization.md § Pillar 2).

A creator paywalls a website-enabled folder to one of their own subscription
tiers by picking the tier on the set's row. The select drives the shared
`FoldersAuthor::paywall_set` through the client's own handle (linux: the crate
direct; windows: the `folders_paywall_set` FFI face): content-key
genesis/re-seal + a `content.read{folder:set}` grant minted to the nest's
web-serve holder + the nest `folders.web_paywall_tier` flag. This covers the
CLIENT-UI *mutation* path (the select click); the full sealed-static-serving
contract the flag gates (teaser / token / revoke) is proven separately by the
tier_3 API test `tests/api/test_web_paywall_folder.py`.

The MUTATION is UI-driven (pick the tier on `folder-paywall-tier-select`);
VERIFICATION reads the nest ground truth via `fauna.folders.list`
(`web_paywall_tier`) — the same external-read shape `test_folder_webdav_toggle.py`
uses for `webdav_enabled` (E2E rule 8: the setting is driven through the UI, the
persisted flag is read back over WS-RPC).

v1 is SET-ONLY (ratified 2026-07-13): the select offers a "Not paywalled"
placeholder only while the set is still public; once paywalled there is no clear
affordance (the nest-side revoke/rotation leg is not shipped yet). This test
asserts the SET transition — the structural sibling of the webdav toggle's serve
transition on owner rows.

No holder seeding: the nest binary self-enrolls its web-serve content-processor
holder at boot (`web_content/holder.rs`, self-approved, x25519 attested), so the
holder `paywall_set` discovers here is the production one.

tier_3 (full stack — `paywall_set` runs custody genesis + the real grant mint
over the live `fauna-nest` binary; a mocked backend can't catch a flow break
between the shared orchestration and the handlers). Dedicated per-test nest
(`test_nest_trust.py`'s `trust_mint_nest` shape) so a tier — which cannot be
un-minted — never makes test order load-bearing on the module-shared nest.
"""

import time
import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.set_names import find_set

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # The apps that render `folder-paywall-tier-select` today (linux led it
    # 2026-07-14; windows lifted it the next day; macOS+iOS lifted the shared
    # FaunaKit `FolderPaywallTierPicker` 2026-07-16; android lifted it 2026-07-19;
    # web lifted it 2026-07-20 — folders.md § Web paywall). Client markers are
    # authoritative here (conftest `pytest_collection_modifyitems`): an unmarked
    # client is DESELECTED, not skipped in-body, so missing coverage stays
    # visible instead of inflating the skip count.
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,  # android lifted 2026-07-19; host-emulator-gated, unrun
    pytest.mark.web,
    # apple only builds the REAL ConversationsSession (which `paywall_set`, like
    # `serve_set`, needs for the MLS/keys custody legs) under this session-wide
    # flag — same reason `test_folder_webdav_toggle.py` carries it. linux/windows
    # build the real session on every e2e login and ignore the marker.
    pytest.mark.real_conversations,
    # tui DID build `folder-paywall-tier-select` 2026-07-30 (`settings/folders.rs`,
    # the expanded website-enabled row body, over `FoldersAuthor::paywall_set` —
    # unit-proven). This test's own SETUP blocker — creating a subscription tier
    # through `subscription-tiers-section`, a profile-page id tui did not paint —
    # landed 2026-08-01 (the SELF Tiers-tab author-management surface,
    # `profile/tiers.rs`; `ui-actual-tui.yaml` profile section now reads COMPLETE
    # apart from the parked knock button). Re-added here.
    pytest.mark.tui,
]


@pytest.fixture
def paywall_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest for the paywall-control test (same shape as
    `test_nest_trust.py`'s `trust_mint_nest` / `test_gated_post_compose.py`'s
    `gated_nest` — own nest, own admin, no shared session state, so test order
    stays non-load-bearing; a tier can't be un-minted on the module-shared nest)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "paywall-nest")
    yield nest
    cleanup()


def _admin_client(nest: dict) -> WsRpcAdminClient:
    """A WS-RPC client on the nest's admin actor — the ground-truth read for the
    per-set `web_paywall_tier` flag (`fauna.folders.list`). The paywalled set is
    the admin's own, so its own list-read returns it."""
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(admin["actor_id_hex"]),
        signing_key=bytes(admin["signing_key"]),
    )


def _folder_row(client: WsRpcAdminClient, name: str) -> dict | None:
    """The nest's own `fauna.folders.list` row for `name` (`None` = absent) —
    by `name_hash`: a sealed set's row rests no plaintext name
    (`helpers/set_names.py`), so a `fs["name"]` match never finds one."""
    reply = client.call("fauna.folders.list", {})
    return find_set(reply.get("folders", []), name)


def _paywalled_tier(client: WsRpcAdminClient, name: str) -> str | None:
    """Nest-authoritative per-set paywall tier (`None` = not paywalled)."""
    row = _folder_row(client, name)
    return None if row is None else row.get("web_paywall_tier")


def _website_enabled(client: WsRpcAdminClient, name: str) -> bool:
    """Nest-authoritative `folders.website_enabled` — the flag the paywall row
    keys on (phase 4 re-keyed the nest gate; the apps followed 2026-09-28 when
    the `mode = "web"` spelling retired)."""
    row = _folder_row(client, name)
    return bool(row and row.get("website_enabled"))


def _wait(pred, timeout: float = 20.0, interval: float = 0.3) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(interval)
    return bool(pred())


@pytest.mark.feature("paid-posts-and-tips")
def test_folder_paywall_select_paywalls_a_website_set(app, paywall_nest):
    """The paywall-to-tier flow, end-to-end through the UI: a creator makes a
    subscription tier and a website folder (the wizard, then the row's
    `folder-website-toggle`), then picks the tier on the set's
    `folder-paywall-tier-select` — driving `FoldersAuthor::paywall_set` and
    flipping the nest `folders.web_paywall_tier` flag.

    The website-enabled row renders the paywall select — keyed on the toggle,
    never on the retired `mode = "web"` spelling, which no app could create
    since the wizard's mode step retired (a gate on the spelling left the
    picker unreachable on every folder a user can make). Set-only in v1: the
    assertion is the SET transition, read back off the nest."""
    # Login as the nest admin (admin ⊇ user — covers tier-create + paywall; the
    # web-serve holder discovery `paywall_set` uses is `fetch_bridge_pubkey`, NOT
    # admin-gated, so a plain user would also do). set_state login, test_nest_trust
    # shape — the client's e2e login must build the REAL ConversationsSession that
    # `build_folders_author` (hence `paywall_set`) needs: linux does it in
    # `conv_backend::start_conversations_session`, windows in
    # `App.BuildE2eConvSessionAsync` (threaded into `ServiceClients.ConvSession`).
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, paywall_nest)

    admin = paywall_nest["admin"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": paywall_nest["url"],
            "secret_hex": bytes(admin["signing_key"]).hex(),
            "handle": "admin",
            "actor_id": admin["actor_id_hex"],
            "device_id": "test-device-paywall",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    time.sleep(1.5)

    # ── Creator: define a subscription tier (the paywall select's options are the
    # creator's own tiers; the nest also validates the tier exists on set).
    tier = f"gold-{uuid.uuid4().hex[:6]}"
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1 My tiers; error={subs.error_text()!r}"
    )

    # ── Creator: a website folder, the way a user makes one — the wizard, then
    # the row's website toggle (web-content-hosting.md — its files serve as a
    # website; paywalling gates them behind the tier). The toggle is the only
    # door to a website folder since the wizard's mode step retired.
    b = app.backups
    name = f"paywall-web-{uuid.uuid4().hex[:6]}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)
    assert b.website_toggle_visible(), (
        f"folder-website-toggle must render on the expanded owner row of "
        f"{name!r}; error={app.error_text()!r}"
    )
    b.toggle_website()

    gt = _admin_client(paywall_nest)
    with gt:
        assert _wait(lambda: _website_enabled(gt, name)), (
            f"flipping folder-website-toggle must persist website_enabled on "
            f"{name!r}; error={app.error_text()!r}"
        )
        assert _paywalled_tier(gt, name) is None, (
            f"a freshly-created website set is not paywalled; error={app.error_text()!r}"
        )

        # ── The website-enabled row renders the paywall select, enabled (a tier
        # exists). The flip re-renders the row from the refreshed snapshot; an
        # app that rebuilds the list on that tick collapses the row, so re-expand
        # only when the select is actually gone (`find_and_expand_folder`
        # TOGGLES — the trap `helpers/folder_content` documents).
        if not _wait(lambda: b.paywall_select_visible(), timeout=10.0):
            b.find_and_expand_folder(name)
        assert _wait(lambda: b.paywall_select_visible()), (
            f"a website-enabled row must render folder-paywall-tier-select; "
            f"error={app.error_text()!r}"
        )
        assert b.paywall_select_enabled(), (
            "the paywall select must be enabled once the creator has a tier; "
            f"error={app.error_text()!r}"
        )

        # ── MUTATION (UI): pick the tier → paywall_set(tier).
        b.set_paywall_tier(tier)

        # ── VERIFICATION (nest ground truth): the per-set flag now names the tier.
        assert _wait(lambda: _paywalled_tier(gt, name) == tier), (
            f"picking the tier on folder-paywall-tier-select must set "
            f"web_paywall_tier={tier!r} on {name!r}, got "
            f"{_paywalled_tier(gt, name)!r}; error={app.error_text()!r}"
        )
        assert not app.has_error(), f"paywall raised an error: {app.error_text()!r}"
