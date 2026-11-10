"""tier_3: Fauna Kids — the seven outcomes of the `kids-app` feature page, each
driven through the kids build's own UI.

`docs/goal/behavior/account-age-band.md` § The account age band, the kids-app
bullet: item (3) the eligibility verdict (an account nobody supervises is refused
past sign-in by one explanatory surface with a sign-out), item (4) the surface set
D-K5 (*Kept*, *Excised*, *Floored*), and `docs/goal/architecture/dynamic-features.md`
§ Compile-time excision (the kids floor). The page's columns are
`android-kids` and `ios-kids` (`feature-catalog.md` § Cell semantics, the
kids-columns ruling): the parent app's driver launching the kids identity, its
records in its own ledger file.

**Which runs reach these tests.** A kids column's applicable set is its parent's,
so the module is marked `ios` and `android`; but on the seven main builds every
outcome here is false by design (the page declares the seven absent), so a run
whose app axis is the parent app itself declares the absence and skips
(`_kids_columns_only`). Only `--app ios-kids` / `--app android-kids` drives them.

**The session user is a ward.** Under a kids run conftest admits the session
`test_user` under a guardian of its own (`conftest.py::test_user`, the
`_kids_session_guardian` branch), so `logged_in_app` lands past the eligibility
gate; the ineligible journey signs in a fresh, unsupervised actor instead.

**What a journey cannot observe, said once.** Outcome 3's "opens nothing when
tapped": the kids build discards a tapped link (`OpenURL.open`'s kids arm and the
root's `openURL` handler), but a DEBUG main build under the harness suppresses
link opening too, so there is no app-side observable that tells the two apart; the
witness is that the link is painted as message text with no preview card even when
a resolved preview is staged for it. Outcome 5's "nothing is requested from an
outside site": the witness is that the remote picture stays the placeholder with no
reveal affordance — the only path that would fetch it.
"""

from __future__ import annotations

import subprocess
import uuid

import pytest

from actions.api_actor import ApiActor
from conftest import KIDS_PARENT, _login_app_as, _make_user
from helpers import budgets
from helpers.app_surface import declared_absence, skip_environment
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.ios, pytest.mark.android]

_AGE_BAND_DOC = "docs/goal/behavior/account-age-band.md § The account age band"


@pytest.fixture(autouse=True)
def _kids_columns_only(request):
    """The page's outcomes exist only in the kids build: on a run whose app axis
    is the parent app (`--app ios`, `--app android`) declare the absence before
    any app is launched — the page declares all seven main columns absent, citing
    the same section. Autouse, so it runs ahead of the `app` fixture family."""
    callspec = getattr(request.node, "callspec", None)
    name = callspec.params.get("app") if callspec is not None else None
    if name not in KIDS_PARENT:
        declared_absence(
            name or "this app",
            capability="the Fauna Kids build's surface set (a kids column only)",
            doc=_AGE_BAND_DOC,
        )


def _fresh_actor_hex() -> str:
    return uuid.uuid4().hex + uuid.uuid4().hex  # 64 hex chars = 32 bytes


def _viewer(nest_instance, user) -> ApiActor:
    return ApiActor(
        nest_instance["url"], user["token"], user["actor_id_hex"], bytes(user["signing_key"]),
    )


def _wait_present(app, element_id: str, why: str, budget: float = budgets.UI_SETTLE_S) -> None:
    wait_until(
        lambda: app.driver.count(element_id) >= 1,
        budget,
        diagnose=lambda: f"{why}: {app.driver.diagnose(element_id)}; error={app.error_text()!r}",
    )


def _assert_absent(app, element_id: str, why: str) -> None:
    assert app.driver.count(element_id) == 0, (
        f"{why}: {element_id} is present in the kids build "
        f"({app.driver.diagnose(element_id)})"
    )


# ── 1. Own store identity; only a supervised account gets past sign-in ────────


@pytest.mark.feature("kids-app")
def test_an_unsupervised_account_is_told_why_and_can_only_sign_out(app, request, nest_instance):
    """Outcome 1. The kids build runs under its own store identity, and an
    account no guardian supervises is stopped at the one explanatory surface —
    no tab shell behind it — whose only affordance is the ordinary sign-out,
    which returns to onboarding."""
    d = app.driver
    if d.is_ios():
        from drivers.ios import KIDS_BUNDLE_ID

        assert d.BUNDLE_ID == KIDS_BUNDLE_ID, (
            f"the ios-kids column must launch the kids identity {KIDS_BUNDLE_ID!r}, "
            f"not {d.BUNDLE_ID!r}"
        )
        udid = getattr(d, "_udid", None)
        if udid:
            installed = subprocess.run(
                ["xcrun", "simctl", "get_app_container", udid, KIDS_BUNDLE_ID, "app"],
                capture_output=True, text=True, timeout=60,
            )
            assert installed.returncode == 0 and installed.stdout.strip(), (
                f"{KIDS_BUNDLE_ID} is not installed on the simulator {udid}: "
                f"{installed.stderr.strip()!r}"
            )

    adult = _make_user(nest_instance)  # no guardian: an unsupervised account
    _login_app_as(app, request, nest_instance, adult)

    _wait_present(app, "kids-ineligible-message", "an unsupervised account must meet the refusal")
    assert d.get_text("kids-ineligible-message").strip(), "the refusal explains nothing"
    for shell in ("conversations-tab", "contacts-tab", "more-tab", "main-tab-view"):
        _assert_absent(app, shell, "the refusal replaces the whole signed-in shell")

    d.click("sign-out-button")
    _wait_present(app, "sign-out-confirm-button", "sign-out asks to confirm")
    d.click("sign-out-confirm-button")
    _wait_present(app, "create-identity-button", "signing out returns to onboarding", budgets.RPC_ROUNDTRIP_S)
    _assert_absent(app, "kids-ineligible-message", "the refusal is gone once signed out")


# ── 2. The kept planes, and nothing else ──────────────────────────────────────


@pytest.mark.feature("kids-app")
def test_the_app_keeps_conversations_contacts_folders_calendar_family_and_settings_only(logged_in_app):
    """Outcome 2. The tab shell is Conversations, Contacts and More; More holds
    the kept pages and no bridges; Settings keeps folders and the device basics
    and has no web publishing, no subscriptions, no connected apps, no bridge or
    mail settings and no personalization. Feed and search are not in the build."""
    app = logged_in_app
    d = app.driver

    for tab, page_anchor in (
        ("contacts-tab", "contacts-search-field"),
        ("conversations-tab", "new-conversation-button"),
        ("more-tab", "settings-tab"),
    ):
        d.click(tab)
        _wait_present(app, page_anchor, f"{tab} must open its page")

    for excised in ("feed-tab", "search-tab", "search-query-field"):
        _assert_absent(app, excised, "the feed and search are excised")

    # On the More page: the kept planes are listed, bridges is not.
    for kept in ("events-tab", "media-tab", "backups-tab", "settings-tab"):
        _wait_present(app, kept, f"More must list the kept {kept}")
    _assert_absent(app, "bridges-tab", "every bridge is excised (D-K5)")

    d.click("settings-tab")
    _wait_present(app, "settings-nav-row[folders]", "Settings keeps folders")
    for kept in ("settings-nav-row[devices]", "family-tab"):
        _wait_present(app, kept, f"Settings keeps {kept}")
    for excised in (
        "settings-nav-row[web]",
        "settings-nav-row[subscription-settings]",
        "settings-nav-row[connected-apps]",
        "settings-nav-row[nostr]",
        "settings-nav-row[atproto]",
        "settings-nav-row[mail-settings]",
        "settings-nav-row[personalization]",
        "settings-nav-row[labeler-catalog]",
    ):
        _assert_absent(app, excised, "Settings is reduced to the kept planes (D-K5)")


# ── 3. A link is inert text with no preview card ─────────────────────────────


LINK_URL = "https://example.com/article"
LINK_BODY = f"[{LINK_URL}]({LINK_URL})"


@pytest.mark.feature("kids-app")
def test_a_link_in_a_message_is_plain_text_with_no_preview_card(logged_in_app):
    """Outcome 3. A message whose body is a bare link renders the link as the
    message's text and paints no preview card — even with a resolved preview
    staged for that very url, the state that paints the card on the main build
    (`test_conversations_link_preview.py`)."""
    app = logged_in_app
    conv = app.conversations
    d = app.driver

    thread_id = conv.inject_and_resolve_thread(
        rail="FaunaMls", sender="kidlink-friend@self-nest.test", subject=None, body=LINK_BODY,
    )
    conv.seed_resolved_link_preview(
        url=LINK_URL, title="Example Article Title",
        description="A short description of the example article.", image_hash="ab" * 32,
    )
    conv.open_thread_by_id(thread_id)

    _wait_present(app, "dm-message-text", "the message must render")
    text = d.get_text("dm-message-text", index=0)
    assert "example.com/article" in text, (
        f"the link must be painted as the message's text; got {text!r}"
    )
    for excised in ("link-preview-card", "link-preview-title", "link-preview-image"):
        _assert_absent(app, excised, "a link shows no preview card (D-K5: outbound links excised)")


# ── 4. A profile only from a confirmed contact, with no Follow and no Tiers ───


@pytest.mark.feature("kids-app")
def test_a_profile_opens_only_from_a_confirmed_contact_with_no_follow_or_tiers(
    logged_in_app, nest_instance, test_user,
):
    """Outcome 4. Tapping a contact the child has only accepted opens nothing; a
    confirmed contact's row opens their profile, which has no Follow button and
    no Tiers tab; and a Find User hit — a non-contact — offers no way to open a
    profile (the lookup and the knock stay)."""
    app = logged_in_app
    d = app.driver
    viewer = _viewer(nest_instance, test_user)
    accepted, confirmed = _fresh_actor_hex(), _fresh_actor_hex()
    # The edges are the precondition, set over the wire; what is under test is
    # what the rows do when tapped.
    viewer.accept_knock(accepted)
    viewer.accept_knock(confirmed)
    viewer.confirm_contact(confirmed)

    app.contacts.navigate()
    _wait_present(app, accepted, "the accepted contact's row must show", budgets.RPC_ROUNDTRIP_S)
    _wait_present(app, confirmed, "the confirmed contact's row must show", budgets.RPC_ROUNDTRIP_S)

    d.click(accepted)
    _assert_absent(app, "profile-view", "an accepted (not confirmed) contact's row opens no profile")
    assert d.count("contact-row") >= 2, "the roster must still be showing after the inert tap"

    d.click(confirmed)
    _wait_present(app, "profile-view", "a confirmed contact's row opens their profile")
    _wait_present(app, "profile-handle", "the profile header must paint")
    _assert_absent(app, "profile-follow-button", "a profile shows no Follow button")
    _assert_absent(app, "profile-tiers-tab", "a profile shows no Tiers tab")

    # Find User: a raw actor id resolves offline into a result row, which offers
    # no open-profile button in the kids build.
    stranger = _fresh_actor_hex()
    app.contacts.navigate()
    _wait_present(app, "contact-actor-id-field", "Find User must be on the contacts page")
    d.clear_and_type("contact-actor-id-field", stranger)
    d.click("contact-actor-id-lookup")
    _wait_present(app, "contact-actor-id-result", "a Find User lookup must show its hit")
    _assert_absent(
        app, "contact-actor-id-open-profile-button",
        "a Find User hit is a non-contact, whose profile is excised",
    )


# ── 5. A remote picture stays a placeholder ──────────────────────────────────


REMOTE_IMG_URL = "https://example.com/cat.png"
REMOTE_IMG_BODY = f"Before ![cat]({REMOTE_IMG_URL}) after"


@pytest.mark.feature("kids-app")
def test_a_remote_picture_stays_a_placeholder_with_no_way_to_fetch_it(logged_in_app):
    """Outcome 5. A message linking a picture from elsewhere paints the
    placeholder, and the message offers no button to fetch it — the reveal is
    the one path that would request it from the outside site."""
    app = logged_in_app
    app.conversations.inject_and_open_thread(
        rail="FaunaMls", sender="kidimg-friend@self-nest.test", subject=None, body=REMOTE_IMG_BODY,
    )
    _wait_present(app, "dm-message-text", "the message must render")
    _wait_present(app, "doc-remote-image", "the remote picture must paint as its placeholder")
    _assert_absent(app, "load-remote-content-button", "there is no button to fetch a remote picture")


# ── 6. Sign-in never offers to set up or retire a nest ───────────────────────


# A name with no delegation inside a held zone: the handle check reads it
# DomainAvailable ("It sits inside …", `actions/onboarding.py::go_to_dns_config`),
# the outcome that takes the main build on into DNS and hosting setup.
_SETUP_HANDLE = "alice@kids-e2e-nobody.example.com"


@pytest.mark.feature("kids-app")
def test_signing_in_never_offers_to_set_up_a_nest(app):
    """Outcome 6. A handle on a domain nobody runs a nest for — the outcome that
    takes the main build into DNS and hosting setup — keeps the kids build on
    handle entry: no DNS page, no hosting page, no provisioning. A child joins
    their guardian's nest by invite."""
    d = app.driver
    onboarding = app.onboarding
    onboarding.go_to_handle_entry()
    onboarding.fill_handle(_SETUP_HANDLE)
    onboarding.run_handle_check(timeout=20)
    msg = d.get_text("handle-message-area").strip()
    if not ("appears to be available" in msg or "is available" in msg or "It sits inside" in msg):
        # The same environment gate `go_to_dns_config` applies: no app work makes
        # an unreachable resolver read a domain available.
        skip_environment(
            "handle-check did not return DomainAvailable for the test domain — "
            f"message-area text: {msg!r}"
        )
    d.click("handle-entry-continue-button")
    _wait_present(app, "handle-entry-continue-button", "the kids build stays on handle entry")
    for excised in (
        "dns-buy-domain-checkbox",
        "dns-provider-row",
        "dns-set-up-later-button",
        "vps-provider-row",
        "launch-retire-button",
    ):
        _assert_absent(app, excised, "nest setup and retirement are excised from sign-in")


# ── 7. The four content categories stay blocked ──────────────────────────────


_FLOOR_CATEGORIES = ("commercial", "nsfw", "phishing", "spam")


@pytest.mark.feature("kids-app")
def test_the_four_content_categories_stay_blocked_whatever_the_guardian_set(logged_in_app):
    """Outcome 7. The session ward's guardian has set nothing — its policy is
    the unsupervised-equivalent default, every category allowed — yet a message
    labelled with any of the four guardian categories renders the block notice
    in place of its body, while a clean message renders as written. The floor is
    compiled into the kids build (`KIDS_CONTENT_FLOOR`) and composes
    strictest-wins with the account's policy."""
    app = logged_in_app
    d = app.driver
    sender = "kidfloor-friend@self-nest.test"
    clean = "an ordinary message from a friend"

    app.conversations.navigate()
    app.conversations.inject_and_open_thread(rail="FaunaMls", sender=sender, body=clean)
    for category in _FLOOR_CATEGORIES:
        app.conversations.inject_inbound_for_test(
            rail="FaunaMls", sender=sender,
            body=f"this {category} body must never render",
            labels=[{"category": category, "confidence_per_mille": 900}],
        )

    expected = 1 + len(_FLOOR_CATEGORIES)
    wait_until(
        lambda: d.count("dm-message-text") + d.count("content-policy-blocked-notice") >= expected,
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"expected {expected} rendered messages; dm-message-text="
            f"{d.count('dm-message-text')} content-policy-blocked-notice="
            f"{d.count('content-policy-blocked-notice')}; error={app.error_text()!r}"
        ),
    )
    assert d.count("content-policy-blocked-notice") == len(_FLOOR_CATEGORIES), (
        f"each of {_FLOOR_CATEGORIES} must render the block notice "
        f"({d.diagnose('content-policy-blocked-notice')}); error={app.error_text()!r}"
    )
    assert d.count("dm-message-text") == 1, (
        f"only the clean message's body may render; dm-message-text count="
        f"{d.count('dm-message-text')}"
    )
    assert d.get_text("dm-message-text", index=0).strip() == clean
