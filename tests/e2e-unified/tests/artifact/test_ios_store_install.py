"""The iOS app as TESTFLIGHT delivered it: obtained from the store, then driven.

Subject: the app TestFlight installed on a physical iPhone, a Release build
Apple re-signed for delivery. It is not the DEBUG ``.app`` the harness puts on a
simulator, and not a signed ``.ipa`` installed by Xcode or ``devicectl`` (that
is a developer install, which never colours this outcome). TestFlight installs
only on a device, so a simulator never counts (``installers/ios.md``
§ Implementation status today). The ruling, its reasons and the human inch:
``docs/goal/architecture/feature-catalog.md`` § Implementation status today,
the bullet on how the iOS store install is driven.

**Why a driver of its own.** The harness's iOS driver talks to the in-process
TestAgent, which convention 15 compiles out of every Release build. The store
driver (``apps/fauna-apple/StoreDriver/``, built by ``just ios-store-driver``)
is an XCUITest runner with a bundle id and signing of its own, and it reaches
the app only through the accessibility identifiers the views set outside any
``#if DEBUG`` (``e2e-automation-surface-gating.md`` § Implementation status
today). It has none of the agent's in-process powers, so the journey signs in
through the app's own UI. Host half: ``helpers/ios_store_driver.py``.

**The account: this machine's own tester, on ``test.fauna.build``.** The same
ruling as the android witness, in the shared helper
``helpers/store_install_account.py``: a store build pins no nest and refuses
cleartext, the acceptance nest is humans-only, and the release onboarding
registers a fresh identity only with an invite code — so the journey imports an
admitted identity, types its handle and continues, the steps a store reviewer
takes from the review notes. The identity is ``macos-tester``, minted and kept
on this machine (``just org-tester-mint``) and admitted on the box by the
provisioning drive from the tracked registry of public keys
(``org-nests.md`` § fauna.build → *Accounts*, the witness-host tester); no
secret travels. The host reads the identity file and hands the handle and the
64-hex seed to the runner's environment; the seed is cut out of everything the
runner reports (``run_test``'s ``secrets``).

**The subject is asserted on two facts, because either alone is matched by
something else.** First, TestFlight installed it: the app was absent when the
module started, the runner pressed Install in TestFlight, and TestFlight showed
the app installed. Second, the device does not class the install as a
developer app (``devicectl``), so it was not signed with a development identity
or put there by Xcode. A developer install of the same ``.ipa`` matches the
first fact's end state but not the second. A TestFlight install left over from
an earlier run matches the second but not the first.

**Environment.** It needs ``ios_store_driver.DEVICE_NEED``. Without it, every
test here skips as ``skip_environment`` and names what is missing. The sign-in
leg also needs this machine's tester identity,
``~/.config/fauna/staging-box/test.fauna.build/accounts/macos-tester.json``
(``helpers.staging_box.account_path``), minted here once by ``just
org-tester-mint`` and registered in ``tests/e2e-unified/org-testers.toml``;
until it is minted and registered the sign-in test skips as
``skip_environment`` naming the recipe. The reviewers' identity files stay on
the dev host alone (``org-nests.md`` § fauna.build → *Accounts*: one copy) and
are never carried here.

**Not repeatable on the same device without an uninstall.** If the app is
already installed when the module starts, the module refuses it: the first
fact is "this run saw TestFlight install it". To run again, delete the app from
the iPhone so TestFlight installs it afresh.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from common import get_repo_root
from helpers import ios_store_driver as store
from helpers.ios_store_driver import run_test
from helpers.app_surface import skip_environment
from helpers.staging_box import SEEDING_ACCOUNT
from helpers.store_install_account import APP_STORE_ACCOUNT, STORE_BOX, StoreAccount, witness_account
from i18n.strings import S

# tier_4: the subject is a shipped client artifact as its channel delivered it
# (`testing.md` § The four-tier taxonomy, point 4's shipped-client-artifact arm).
pytestmark = [pytest.mark.tier_4, pytest.mark.ios]

#: The name TestFlight lists the app under; the on-device name by default.
APP_NAME_ENV = "E2E_STORE_APP_NAME"


def _environment_or_fail(outcome: store.RunnerOutcome) -> None:
    """Map a runner skip to an environment skip, and any other non-pass to a
    failure that carries the runner's own messages (convention 6)."""
    if outcome.result == "Passed":
        return
    if outcome.result == "Skipped" and outcome.state in store.ENVIRONMENT_STATES:
        skip_environment(f"{'; '.join(outcome.messages)} ({store.DEVICE_NEED})")
    pytest.fail(f"the store driver's run did not pass — {outcome.describe()}", pytrace=False)


@pytest.fixture(scope="module")
def device() -> store.Device:
    """The paired iPhone this run drives, or an environment skip naming what
    the witness needs."""
    try:
        picked = store.pick_device()
    except FileNotFoundError:
        skip_environment(f"`xcrun devicectl` is not available on this box; the witness needs {store.DEVICE_NEED}")
    if picked is None:
        skip_environment(f"no paired iOS device; the store-install witness needs {store.DEVICE_NEED}")
    if picked.developer_mode and picked.developer_mode != "enabled":
        skip_environment(
            f"Developer Mode is {picked.developer_mode!r} on {picked.name or picked.udid}; "
            f"the runner cannot start without it ({store.DEVICE_NEED})"
        )
    return picked


@pytest.fixture(scope="module")
def repo(device) -> Path:
    """The repo, once the runner is known to be built — read, never built here
    (convention 9's prebuild bullet): the build is the recipe's."""
    root = Path(get_repo_root())
    if store.find_xctestrun(root) is None:
        pytest.fail(
            f"the store driver is not built under {root / store.DRIVER_PRODUCTS} — run "
            f"`just ios-store-driver` first (it signs with the development profile "
            f"`Fauna Development social.fauna.storedriver`, which must list the device)",
            pytrace=False,
        )
    return root


def _runner_env(account: StoreAccount | None = None) -> dict[str, str]:
    env: dict[str, str] = {}
    if account is not None:
        env["FAUNA_STORE_ACCOUNT_HANDLE"] = account.handle
        env["FAUNA_STORE_ACCOUNT_SECRET"] = account.secret_hex
    if os.environ.get(APP_NAME_ENV, "").strip():
        env["FAUNA_STORE_APP_NAME"] = os.environ[APP_NAME_ENV].strip()
    return env


@pytest.fixture(scope="module")
def store_install(device, repo) -> dict:
    """Obtain the app the way a tester does: open TestFlight, press Install,
    wait for TestFlight to show it installed. Returns the device's reading of
    the install.

    An app already on the device is refused: this run must see TestFlight make
    the install, or the first subject fact has no observation behind it.
    """
    before = store.install_classification(device)
    assert not before["installed"], (
        f"{store.FAUNA_BUNDLE_ID} is already installed on {device.name or device.udid} "
        f"({before}) — this run cannot see TestFlight install it. Delete the app "
        f"from the iPhone and run again."
    )
    outcome = run_test(repo, device, store.OBTAIN_TEST, _runner_env())
    _environment_or_fail(outcome)
    after = store.install_classification(device)
    assert after["installed"], (
        f"TestFlight showed the app installed, but devicectl lists no "
        f"{store.FAUNA_BUNDLE_ID} on the device: {after}\n{outcome.log_tail}"
    )
    return {**after, "obtained_by_testflight_this_run": True}


def _assert_the_subject(install: dict) -> None:
    """The two subject facts, asserted by every test that colours anything."""
    assert install.get("obtained_by_testflight_this_run"), (
        "this run did not see TestFlight install the app — it is not the store install"
    )
    assert not install["listed_as_developer_app"] and install["built_by_developer"] is not True, (
        f"the device classes the install as a developer app ({install}): it was "
        f"signed with a development identity or installed by Xcode, not delivered by TestFlight"
    )


def test_testflight_installs_the_store_signed_app(store_install):
    """The obtain half and the subject: TestFlight installed the app, and the
    device reads it as a store delivery, not a developer app."""
    _assert_the_subject(store_install)


@pytest.mark.feature("get-the-app")
def test_the_testflight_installed_app_signs_in_to_the_feed(store_install, device, repo):
    """``get-the-app`` outcome 1, the iPhone arm: installed from TestFlight,
    the app opens and you are signed in to your account.

    It re-asserts its own subject before its outcome, the linux module's
    lesson (``test_linux_installed_product.py``): a narrowed run must not
    colour this cell green while driving something else.
    """
    _assert_the_subject(store_install)
    account = witness_account(APP_STORE_ACCOUNT)
    outcome = run_test(repo, device, store.SIGN_IN_TEST, _runner_env(account),
                       secrets=(account.secret_hex,))
    _environment_or_fail(outcome)


# ── store-install: the store flavor's absences ────────────────────────────
#
# ``docs/features/store-install.md``: the App Store takes the ``store-safe``
# flavor, so the TestFlight delivery carries neither the money plane nor the
# in-person share (``dynamic-features.md`` § The store flavors). Each node runs
# one runner method that reaches the screen a full build would put the control
# on, proves it is there by an id the store flavor keeps, and asserts the excised
# ids absent. Which ids, and the pin that none of them is an id nothing ever
# drew: the ``store-absent`` block of ``StoreDriverTests.swift`` and
# ``test_ios_store_driver_pins.py``. Every method signs in through the app's UI
# when the install opens on onboarding, so a narrowed run of one node stands
# alone. The buyer halves of outcomes 2 and 3 (a priced tier or a sold post shown
# without its price) need content the store build cannot make: they read the
# box's seeding account, which the provisioning drive seeds from a full build
# with a priced tier and a sold post, opened through Find User as any reader
# opens an author.

#: The author the buyer halves read (``staging_box.SEEDING_ACCOUNT`` on the box).
SELLER_HANDLE = f"{SEEDING_ACCOUNT}@{STORE_BOX}"


def _run_store_outcome(test: str, store_install, device, repo) -> None:
    """Re-assert the subject, then run one store-install method signed in as
    this machine's tester."""
    _assert_the_subject(store_install)
    account = witness_account(APP_STORE_ACCOUNT)
    env = _runner_env(account)
    # The compose audience picker's answers carry no id; the runner reads them by
    # the app's own strings (outcome 3).
    env["FAUNA_STORE_GATE_PUBLIC_LABEL"] = S.feed.post.gate_public
    env["FAUNA_STORE_GATE_SELL_LABEL"] = S.feed.post.gate_sell
    # The buyer halves of outcomes 2 and 3 read this author's seeded content.
    env["FAUNA_STORE_SELLER_HANDLE"] = SELLER_HANDLE
    outcome = run_test(repo, device, test, env, secrets=(account.secret_hex,))
    _environment_or_fail(outcome)


@pytest.mark.feature("store-install")
def test_the_testflight_install_offers_no_payment_provider_or_code(store_install, device, repo):
    """``store-install`` outcome 1, the iPhone arm: no payment provider to
    connect (own profile → Tiers), no code to hand out, none to redeem
    (Settings → Subscriptions)."""
    _run_store_outcome(store.STORE_NO_PROVIDER_NO_CODE_TEST, store_install, device, repo)


@pytest.mark.feature("store-install")
def test_the_testflight_install_tier_editor_has_no_price(store_install, device, repo):
    """``store-install`` outcome 2, the author half: the tier editor has no
    price, price hint or payment link (opened and cancelled, nothing saved)."""
    _run_store_outcome(store.STORE_TIER_EDITOR_NO_PRICE_TEST, store_install, device, repo)


@pytest.mark.feature("store-install")
def test_the_testflight_install_never_offers_to_sell_a_post(store_install, device, repo):
    """``store-install`` outcome 3, the author half: the compose audience picker
    has no "Sell this post…" answer and no sale control is painted (nothing
    posted)."""
    _run_store_outcome(store.STORE_COMPOSE_NO_SALE_TEST, store_install, device, repo)


@pytest.mark.feature("store-install")
def test_the_testflight_install_shows_a_priced_tier_without_its_price(store_install, device, repo):
    """``store-install`` outcome 2, the buyer half: the seeded author's priced
    tier shows on their profile's Tiers tab as an ordinary tier, with no price
    or payment link (nothing subscribed to)."""
    _run_store_outcome(store.STORE_PRICED_TIER_NO_PRICE_TEST, store_install, device, repo)


@pytest.mark.feature("store-install")
def test_the_testflight_install_shows_a_sold_post_without_its_price(store_install, device, repo):
    """``store-install`` outcome 3, the buyer half: the seeded author's sold post
    shows on their Posts tab as a gated post with its badge, and no price,
    payment link or buy button (nothing bought)."""
    _run_store_outcome(store.STORE_SOLD_POST_NO_PRICE_TEST, store_install, device, repo)


@pytest.mark.feature("store-install")
def test_the_testflight_install_shows_no_tips_or_zap_signer(store_install, device, repo):
    """``store-install`` outcome 4: no tip surface on the feed's post cards, no
    zap signer on Settings → Nostr."""
    _run_store_outcome(store.STORE_NO_TIPS_NO_ZAP_SIGNER_TEST, store_install, device, repo)


@pytest.mark.feature("store-install")
def test_the_testflight_install_leaves_out_the_in_person_share(store_install, device, repo):
    """``store-install`` outcome 5, the App Store half: Settings → Folders paints
    neither the offline-share panel nor the peer-transfer readings."""
    _run_store_outcome(store.STORE_NO_IN_PERSON_SHARE_TEST, store_install, device, repo)
