"""tier_3 e2e: a committed deployment-seed rotation re-pins SILENTLY.

``docs/goal/architecture/nest/box-recovery.md`` § Deployment-seed rotation →
*Client acceptance — re-pin on a verified chain*: on a channel-binding identity
mismatch the client fetches the box's rotation chain over the same connection
and — when a valid chain links its pinned identity to the live-proven head —
re-pins with **no warning surface**, storing ``(head, seq)``. This is the
journey that turns rotation from a self-inflicted outage into a ceremony.

**The whole observable ceremony, per the 131st pass's lesson:** the client
TOFU-pins the real identity, the box rotates underneath it (in-process
adoption, original PID alive — pinned by ``api/test_nest_rotation_chain.py``),
and the relaunch lands **in the app** with the pin now naming the successor.
Before the acceptance landed, this exact journey blocked on the
``launch_identity_changed`` warning — which is also this test's failure shape,
so a regression diagnoses itself.

The refusal half of the ceremony ("a superseded key's binding is refused after
convergence") and the fork/no-bridge verdicts are pinned at tier_1
(``fauna_client_core::nest_trust::rotation_tests`` — the ancestor-refusal,
fork-clause and harvested-chain cases), where every branch is reachable; this
module pins the end-to-end wiring over a real nest binary, real TLS, and a real
app relaunch.

Client arm: **tui leads** (the lead app); linux joins with the same harness
(the identical mechanics are proven by ``test_nest_identity_pin.py``, whose
launch-harness journey this mirrors). Web cannot reach this arm in e2e — its
origin is never TLS, so there is no binding for a pin to disagree with (that
acceptance is unit-pinned in ``nest_trust``'s ceremony tests). The apple legs
join by extending ``_SUPPORTED_APPS`` exactly as they did for the pin test.

Latency-independent (convention 14): the one wait beyond the shared launch
polls is the box's in-process adoption — the sanctioned named-budget deadline
poll, copied from ``api/test_nest_rotation_chain.py``.
"""

import contextlib
import json
import secrets
import time

import pytest

from common.cred_store import requires_secret_service
from common.keyring import secret_service_available
from helpers.app_surface import skip_environment
from common.launch_harness import make_launch_harness, reached_authenticated_app
from conftest import _trust_seeder, get_available_apps

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui]

_SUPPORTED_APPS = ("tui", "linux")

# Element IDs (tests/e2e-unified/ui.yaml § launch_identity_changed).
IDENTITY_WARNING = "nest-identity-changed-warning"

_ROTATE_KIND = "fauna.admin.deployment_seed.rotate"

# Generous ceiling for the in-process generation restart (reply-flush delay +
# WS 1001 drain + worker cancel + rebind) — the same named budget the chain API
# tests use; a green run pays only the real teardown.
_ADOPTION_BUDGET_S = 60.0


def _repin_clients():
    available = get_available_apps()
    return [c for c in _SUPPORTED_APPS if c in available]


@pytest.fixture(params=_repin_clients())
def repin_client(request):
    return request.param


@pytest.fixture(autouse=True)
def _require_credential_persistence(repin_client):
    """linux's adapter runs a private Secret Service daemon of its own and skips
    only where the box cannot supply one; tui's file backend needs nothing and
    never skips."""
    if requires_secret_service(repin_client) and not secret_service_available():
        skip_environment(
            "linux's real-keyring launches run a private gnome-keyring-daemon, which this box cannot supply"
        )


def _nest_id_hex(nest_url):
    """The identity the box serves right now (hex), over an anonymous socket."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_url) as anon:
        info = anon.call("fauna.nest.info", {})
    return info["nest_id"]


def _poll_until_serves(nest_url, expected_hex, tag):
    """Deadline-poll ``fauna.nest.info`` until the box serves ``expected_hex``,
    tolerating the re-enter window's refused connections."""
    deadline = time.monotonic() + _ADOPTION_BUDGET_S
    last = None
    while time.monotonic() < deadline:
        try:
            served = _nest_id_hex(nest_url)
        except Exception as e:  # noqa: BLE001 — the window's error shape varies
            last = e
        else:
            if served == expected_hex:
                return
            last = f"still serving {served}"
        time.sleep(0.25)
    raise AssertionError(
        f"{tag}: box did not serve the successor within {_ADOPTION_BUDGET_S}s "
        f"(last: {last!r})"
    )


def _read_pin(driver, nest_url):
    """The pin the installed store holds for ``nest_url`` (hex, or None) — the
    same bridge read ``test_nest_identity_pin.py`` uses."""
    raw = driver.call_machine_method(
        "nest_identity_pin_for_test", json.dumps({"nest_url": nest_url})
    )
    if raw in (None, "", "null"):
        return None
    if not isinstance(raw, str):
        return raw
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw


@pytest.mark.feature("admin-nest")
def test_a_committed_rotation_repins_silently_through_the_chain(
    request, repin_client, rotatable_tls_nest, tmp_path
):
    """Pin → rotate → relaunch → in the app with no warning, pin = successor."""
    nest = rotatable_tls_nest
    app_path = request.getfixturevalue(f"{repin_client}_app_path")
    harness = make_launch_harness(
        repin_client, tmp_path=tmp_path, app_path=app_path, seed_trust=_trust_seeder(request)
    )
    try:
        driver = harness.launch(
            secret_hex=bytes(nest["user"]["signing_key"]).hex(),
            node_url=nest["url"],
            trust=nest,
        )
        # First contact TOFU-pins the box's REAL identity over the live
        # self-signed binding; wait for the app (the online baseline) so the
        # async graduation has settled before we read the pin back.
        reached_authenticated_app(driver, timeout=90)
        before_hex = _nest_id_hex(nest["url"])
        assert _read_pin(driver, nest["url"]) == before_hex, (
            "first contact must have TOFU-pinned the nest's genuine identity — "
            "without that pin there is nothing for the rotation chain to move"
        )

        # The pin store must survive the relaunch or the journey is vacuous.
        if not driver.preserve_state_across_relaunch():
            pytest.skip(
                f"{repin_client} driver cannot pin its client store across a "
                "relaunch, so a TOFU pin cannot survive to be re-pinned"
            )

        # Rotate the live box (admin RPC, python side) and wait for the
        # in-process adoption — from here every channel binding is signed by
        # the successor, so the client's held pin is stale.
        from clients.ws_rpc_admin_client import WsRpcAdminClient

        admin = nest["admin"]
        with WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        ) as ws:
            reply = ws.call(_ROTATE_KIND, {"new_seed": secrets.token_bytes(32).hex()})
        after_hex = bytes(reply["nest_actor_id"]).hex()
        assert after_hex != before_hex, "the box must serve a new identity"
        _poll_until_serves(nest["url"], after_hex, "post-rotation adoption")

        # Relaunch: the launch flow meets the successor identity, fetches the
        # chain over the same connection, verifies old→new + the live binding,
        # and re-pins — landing IN THE APP with no identity-changed surface.
        harness.relaunch()
        try:
            reached_authenticated_app(driver, timeout=90)
        except Exception as e:  # noqa: BLE001 — diagnose the failure shape
            with contextlib.suppress(Exception):
                if driver.is_visible(IDENTITY_WARNING):
                    raise AssertionError(
                        "the relaunch blocked on the identity-changed warning — "
                        "the rotation chain did not silently re-pin "
                        "(box-recovery.md § Client acceptance)"
                    ) from e
            raise

        assert driver.is_absent(IDENTITY_WARNING), (
            "a committed rotation must never surface the identity-changed warning"
        )
        assert _read_pin(driver, nest["url"]) == after_hex, (
            "the pin must now name the successor head the chain licensed"
        )
    finally:
        with contextlib.suppress(Exception):
            harness.teardown()
