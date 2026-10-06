"""The onboarding→launch DNS-credential hand-off, driven through the app UI.

A DNS-provider credential the user verified at the onboarding DNS step must be
**sealed into the admin's `fauna.state.dns` by the launched app**, once it is
authenticated on the new nest, through the same post-onboarding
`DnsManagementMachine::PutCredentials` path the "Fauna controls DNS" mode uses —
one credential store with one writer (`onboarding-provisioning.md` § 4. DNS
configuration, *Capture at onboarding, seal via the launched app*;
`dns-management.md` § Where the credential lives).

**Why this file exists beside the windows one.** `test_onboarding_dns_glue_windows.py`
drives exactly this behaviour, as did a web-only `test_onboarding_dns_glue.py`
until 2026-09-30 — retired when the record became the tip-sealed
`fauna.state.dns` row, which its untrusted plain-HTTP nest can never hold (no pin,
so no tip resolves); this file runs web on a trusted nest. Neither per-app file
can speak for the feature catalog: `features-lint` reads a test as driving
an app only when it opens one through the `app` fixture family or a known driver
factory (`scripts/features_scan.py`), and both reach their app another way — the
web one by importing `local_web` from a module where `create_driver` is called,
the windows one through `_driver_cache`. That refusal is mechanical and correct,
so this is the lift onto the shared `app` fixture rather than a change to the
lint. It is also what makes the outcome *platform-neutral*: the same journey now
runs on whichever apps the run selected, instead of being written twice.

**What it proves that a unit test cannot.** The capture and the seal sit on
opposite sides of the wizard's teardown — the machine is gone by the time an
authenticated client exists — so the only thing that can say the credential
survived the crossing is a run that drives the real claim to `LoggedIn` against
a real nest and then reads the credential back off the `admin-dns` page. The
zone assertion additionally pins the doc's "`PutCredentials` re-runs the
provider `verify()` to (re)derive the covered zones": the injected credential
carries no zone, so a zone on the page can only have come from a `verify()` the
seal ran.

tier_2, not tier_3: the nest binary, the admin claim and the `fauna.state.dns`
plane write are all real; only the **external DNS provider** is faked. The
credential carries the `fake-dns-ok:<zone>` sentinel and the fake-provider
decorator is enabled — via the launch env on native (`FAUNA_DNS_PROVIDER_FAKE`,
set for tui in conftest) and via `enableDnsFakeProviderForTest` on web — so the
seal's `verify()` succeeds offline. The real provider path is unreachable from
the sandbox (it routes through `proxy.fauna.social`), which is why the sentinel
harness exists at all; same shape as `test_admin_dns_managed.py`.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

_e2e_dir = str(Path(__file__).resolve().parent.parent)
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from common.nest import CLAIM_CODE
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier1, pytest.mark.tier_2]

_ZONE = "e2e-onboard.test"
_PROVIDER = "cloudflare"

# A deterministic 32-byte test identity. Which key claims the box is irrelevant
# to the hand-off; a stable one keeps an identity-stage flake from reading as a
# DNS failure.
_SECRET_HEX = "00000000000000000000000000000000000000000000000000000000000000a1"


@pytest.fixture
def unclaimed_dns_nest(request, nest_mode, tmp_path_factory):
    """A fresh, never-claimed nest whose claim code is `common.nest.CLAIM_CODE`.

    The journey below drives the real UI claim against it. `unclaimed` is
    honoured in every nest mode (the docker provider declares it), so the
    ceremony is the same one in a container.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "onboarding-dns-glue-nest", unclaimed=True
    )
    yield nest
    cleanup()


def _claim_to_nat_mode(app, nest) -> None:
    """Import the identity and drive the real claim, parking on `nat_mode_choice`.

    `navigate_to_claim_code_for_known_nest` skips DNS discovery, the same
    shortcut `test_trust_prompt.py` takes for the same reason: the handle probe
    is not what this test is about, and faking it keeps the journey off the
    network.
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(_SECRET_HEX)
    app.driver.wait_for("handle-input", timeout=UI_SETTLE_S)
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest",
        json.dumps([nest["url"], "admin@localhost"]),
    )
    app.driver.wait_for("claim-code-input", timeout=RPC_ROUNDTRIP_S)
    app.driver.clear_and_type("claim-code-input", CLAIM_CODE)
    app.driver.click("claim-code-submit-button")
    # A real WS-RPC claim against a cold nest — the budget is a ceiling, not a
    # target.
    app.driver.wait_for("nat-mode-confirm-button", timeout=RPC_ROUNDTRIP_S)


def _refresh_dns_page(app) -> None:
    """Re-read `admin-dns` — by its own refresh control where the app renders
    one, else by re-navigating.

    Both are real user gestures for the same intent, and which one an app offers
    is a per-app rendering fact, not a behavioural one: windows hydrates the page
    once on load and needs the button, web re-hydrates on navigation. Branching
    on the *element* rather than on the driver type keeps this free of the
    `is_web()`/`is_tui()` test the conventions forbid (convention 3).
    """
    if app.driver.is_visible("admin-dns-refresh-button"):
        app.driver.click("admin-dns-refresh-button")
    else:
        app.admin.navigate_dns()


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_the_verified_dns_credential_is_sealed_by_the_launched_app(app, unclaimed_dns_nest):
    """A credential captured at the DNS step is held by the launched app, and
    carries the zones its `verify()` derived.

    The whole outcome is that it SURVIVES the wizard: the machine that captured
    it has no plane-write capability and, on a fresh provision, runs
    before the nest exists at all — so nothing is sealed until an authenticated
    client reads `captured_dns_credential()` at `LoggedIn` and dispatches
    `PutCredentials`. An app missing that glue loses the credential silently,
    with no error anywhere: the user simply finds the admin DNS page empty and
    has to re-enter what they already typed.
    """
    if app.driver.is_web():
        # The wasm twin of native's launch-env `FAUNA_DNS_PROVIDER_FAKE`. Set
        # before the seal runs; on native the launch env already carries it.
        app.driver.enable_dns_fake_provider()

    _claim_to_nat_mode(app, unclaimed_dns_nest)

    # The localhost claim path never visits `dns_config`, so the wizard captures
    # no credential of its own — inject one carrying the fake-provider sentinel
    # at the last stable point before the terminal confirm. Everything after this
    # line is the production path: `captured_dns_credential()` now yields `Some`
    # at `LoggedIn` exactly as a real managed-publish run would, and the glue
    # under test is what does the rest. Note the payload carries NO zone — the
    # zone asserted below can only come from the seal's own `verify()`.
    app.driver.call_machine_method(
        "set_captured_dns_credential_for_test",
        json.dumps({
            "provider_id": _PROVIDER,
            "fields": {"api-token": f"fake-dns-ok:{_ZONE}"},
        }),
    )

    # Confirming NAT mode is the wizard's terminal step on the admin path
    # (`onboarding.md` § 3b-bis), and reaching `Done` is what fires the seal.
    # `finish_nat_mode` answers the trust interstitial behind it too.
    app.onboarding.finish_nat_mode()

    app.admin.navigate_dns()

    # The seal is fire-and-forget by design (a failure must not paint an error
    # over a completed onboarding), so the page is polled rather than read once.
    # This is a deadline poll on the observable state itself — the credential is
    # either held or it is not — never a fixed wait standing in for one
    # (convention 14). The budget is a ceiling for a loaded box, not an expected
    # duration: a green run returns on the first tick.
    def _credential_held() -> int:
        count = app.admin.dns_credential_count()
        if count:
            return count
        # Not there yet — re-read the page so the next tick sees a fresh
        # hydrate rather than the same stale render.
        _refresh_dns_page(app)
        return 0

    wait_until(
        _credential_held,
        60,
        interval=1,
        diagnose=lambda: (
            "the DNS-provider credential captured during onboarding never reached "
            "fauna.state.dns — admin-dns-credential-item count stayed 0. Either this "
            "app has no LoggedIn launch glue reading captured_dns_credential() and "
            "dispatching PutCredentials, or the fake provider's verify() refused. "
            f"error={app.error_text()!r}"
        ),
    )

    providers = app.admin.dns_credential_providers()
    assert any(_PROVIDER in p for p in providers), (
        f"a credential was sealed but not the captured one: held providers "
        f"{providers!r}, expected one naming {_PROVIDER!r}"
    )

    # The injected payload carried no zone, so a zone on the page proves the
    # seal re-ran the provider's `verify()` to derive coverage — the doc's
    # "what onboarding captured cannot go stale". Without this the test would
    # pass on a glue that stored the credential verbatim and left every domain
    # unmanageable.
    zones = app.admin.dns_credential_zones()
    assert any(_ZONE in z for z in zones), (
        f"the sealed credential carries no derived zone: {zones!r}. PutCredentials "
        f"must re-run verify() and store the zones it reports, or no domain can "
        f"ever be opted into managed mode with it."
    )
