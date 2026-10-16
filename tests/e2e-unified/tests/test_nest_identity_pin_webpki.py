"""tier_3 e2e: the login's pin on a PUBLICLY TRUSTED nest — `connect-and-sign-in`
outcome 14.

``docs/goal/architecture/security.md`` § Transport trust → *The login's pin*,
rule (1): a binding already in hand is never waived by WebPKI. A login on a nest
whose certificate chains to a trusted root still runs the full identity core and
records the identity it verified as the pin, so a later server at the same
address cannot pass as the nest. ``e2e-automation-surface-gating.md`` § The
nest-leg trust seed rules how a rig reaches that branch at all.

**Why this is not an arm of ``test_nest_identity_pin.py``.** That module's arm is
a property of the nest's TLS posture too, but every one of its nests serves the
self-signed floor, on which ``webpki_valid`` is false and the binding was required
before the rule existed — so a journey there is green against the pre-rule code.
This one dials ``webpki_lan_nest``, which serves a harness-CA leaf for the LAN IP
the app dials, and launches the app with ``FAUNA_E2E_NEST_EXTRA_CA_PEM`` naming
that CA — per launch, this test only (the ruling's term 4) — so the app's one
nest-leg verifier (``fauna_ws_substrate::tls_verify::webpki_verifier``) calls the
leaf publicly trusted.

**Half A — the login leaves the pin.** Log in; ``nest_identity_pin_for_test``
names the nest's real identity. The half that is RED against the pre-rule waiver
(``binding.is_none() &&`` removed from ``webpki_waives_binding``): a WebPKI login
with no prior pin then took the waiver and recorded nothing.

**Half B — what that pin buys.** Nest A goes away; nest B — a fresh data dir, so a
different deployment identity — comes up on A's port and A's address, serving a
leaf from the same CA for the same IP, and holding the same user. The relaunch
warns (``nest-identity-changed-warning``) with no Retry CTA; re-trust re-pins B.
Against the pre-rule code half A left no pin, so B was accepted silently.

**The control (``self_signed_control``)** runs the same journey on the same LAN
nest with no leaf installed and no seed: the binding was required there all
along, so it passes on both builds. A difference between the two arms across a
pre-rule/current pair is what proves the journey walks the WebPKI branch.

tui first (lead app); linux joined 2026-10-14 with no app code; macos, ios and windows join in trickle-down — the
seed is read in shared Rust, so each join is a launch-harness leg, not app code.
Web is a declared absence of the seam by construction (browser TLS); its reading
of outcome 14 is the possession proof (``test_nest_identity_pin.py``).
"""

import contextlib

import pytest

from common.auth import register_same_actor_on
from common.cred_store import requires_secret_service
from common.keyring import secret_service_available
from common.launch_harness import make_launch_harness, reached_authenticated_app
from common.nest import lan_ipv4, nest_id_from_data_dir, start_nest, stop_nest
from conftest import _trust_seeder, get_available_apps
from helpers.app_launch import app_launch_config
from helpers.app_surface import skip_environment
from tests.test_nest_identity_pin import (
    IDENTITY_TRUST_BUTTON,
    IDENTITY_WARNING,
    LAUNCH_RETRY,
    _read_pin,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui]

#: The env var the app's nest-leg verifier reads (`tls_verify.rs`
#: `E2E_NEST_EXTRA_CA_ENV`). `test_nest_leg_tls_seam_gating.py` pins this
#: spelling to the Rust one.
NEST_EXTRA_CA_ENV = "FAUNA_E2E_NEST_EXTRA_CA_PEM"

_SUPPORTED_APPS = ("linux", "tui")


def _pin_clients():
    available = get_available_apps()
    return [c for c in _SUPPORTED_APPS if c in available]


@pytest.fixture(params=_pin_clients())
def pin_client(request):
    return request.param


@pytest.fixture(autouse=True)
def _require_credential_persistence(pin_client):
    """linux's adapter runs a private Secret Service daemon of its own and skips
    only where the box cannot supply one (as ``test_nest_identity_pin.py``)."""
    if requires_secret_service(pin_client) and not secret_service_available():
        skip_environment(
            "linux's real-keyring launches run a private gnome-keyring-daemon, which this box cannot supply"
        )


@pytest.mark.feature("connect-and-sign-in")
@pytest.mark.parametrize(
    "webpki_lan_nest", ["webpki", "self_signed_control"], indirect=True
)
def test_a_login_on_a_publicly_trusted_nest_pins_it_and_an_impostor_warns(
    request, pin_client, webpki_lan_nest, nest_binary, tmp_path
):
    nest_a = webpki_lan_nest
    url = nest_a["url"]
    user = nest_a["user"]
    environment = (
        {NEST_EXTRA_CA_ENV: nest_a["ca_pem"]} if nest_a["ca_pem"] is not None else {}
    )
    harness = make_launch_harness(
        pin_client,
        tmp_path=tmp_path,
        **app_launch_config(pin_client, request),
        seed_trust=_trust_seeder(request),
        extra_launch_config={"environment": environment} if environment else None,
    )
    nest_b = None
    try:
        # ── Half A: the login leaves the pin. ──
        driver = harness.launch(
            secret_hex=bytes(user["signing_key"]).hex(), node_url=url, trust=nest_a
        )
        reached_authenticated_app(driver, timeout=90)
        identity_a = nest_id_from_data_dir(nest_a["tmp_dir"])
        pinned = _read_pin(driver, url)
        assert pinned == identity_a, (
            f"[{nest_a['posture']}] a login on {url} must leave the pin naming the "
            f"identity it verified ({identity_a}); the store holds {pinned!r}. On "
            "the webpki arm this is security.md § The login's pin, rule (1) — a "
            "binding in hand is never waived; a None here is the pre-rule waiver "
            "recording nothing"
        )

        if not driver.preserve_state_across_relaunch():
            pytest.skip(
                f"{pin_client} driver cannot pin its client store across a relaunch"
            )

        # ── Half B: an impostor at the same address. ──
        # Only this test's own handles are signalled (`stop_nest` /
        # `start_nest` take the fixture's process, never a name match).
        stop_nest(nest_a, graceful=True)
        nest_b = start_nest(
            nest_binary,
            str(tmp_path / "impostor-nest"),
            port=nest_a["port"],
            serve_tls=True,
            dial_host=lan_ipv4(),
        )
        assert nest_b["url"] == url, (nest_b["url"], url)
        identity_b = nest_id_from_data_dir(nest_b["tmp_dir"])
        assert identity_b != identity_a, "the impostor must hold a different identity"
        if nest_a["ca_pem"] is not None:
            nest_a["install_leaf"](nest_b)
        register_same_actor_on(
            nest_b["port"], user, admin_signing_key=nest_b["admin"]["signing_key"]
        )

        harness.relaunch()
        driver.wait_for(IDENTITY_WARNING, timeout=60)
        assert driver.is_visible(IDENTITY_WARNING), (
            f"[{nest_a['posture']}] a different nest at {url} — "
            + (
                "presenting a certificate this app calls publicly trusted — "
                if nest_a["ca_pem"]
                else ""
            )
            + "must block auto-entry and warn; the pinned identity is what the "
            "login recorded in half A"
        )
        assert driver.is_absent(LAUNCH_RETRY), (
            "a possible-MITM signal must NOT offer a Retry CTA; the "
            "launch_identity_changed page carries no launch-retry-button"
        )

        driver.click(IDENTITY_TRUST_BUTTON)
        reached_authenticated_app(driver, timeout=60)
        repinned = _read_pin(driver, url)
        assert repinned == identity_b, (
            f"after re-trusting, the pin must name the nest now at {url} "
            f"({identity_b}); got {repinned!r}"
        )
    finally:
        with contextlib.suppress(Exception):
            harness.teardown()
        if nest_b is not None:
            with contextlib.suppress(Exception):
                stop_nest(nest_b, graceful=False)
