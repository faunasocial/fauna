"""tier_4: the firewalled-:80 trust
differential.

Against a box that never gets a real certificate (the ``localhost`` identity
never even attempts a public ACME order, so it parks on the always-live
self-signed floor forever — the same end state a real box behind a firewalled
:80 reaches, per ``docker/entrypoint.sh``'s unconditional floor write and
``test_real_hostname_serves_floor_https_under_acme``), a **native/TOFU**
client and a **web/WebPKI** client diverge: the contrast is what pins the
"stuck on Connecting forever, no Admin entry" bug
 to the SPA's trust
model rather than to the nest.

**Native half (this file, below) — verified working, landed.** A linux driver
pointed at the box's ``https://127.0.0.1:<port>`` via the same
``session.node_url`` + ``secret_hex`` injection ``test_mail_client_ui_enable_
docker.py`` already proves works against a self-signed floor: ``trust.rs``'s
TOFU accepts the unpinned first-contact cert unconditionally, the connection
reaches ``Connected``, and ``am-i-admin`` passes (``admin-tab`` visible).

**Web half — IMAGE-GATED, not yet written; do not attempt without a fresh
image.** The entrusted spec
frames this as ONE test asserting both legs against the SAME box; this file
splits it into two for a load-bearing reason found while scoping the web
half, not by choice:

  1. **The web leg needs the SPA served SAME-ORIGIN by the nest itself** — a
     locally-built SPA (this checkout's ``static_dir``) pointed cross-origin
     at the docker box would need the box's CORS allow-list to include the
     local test origin, and a fresh/default nest's ``cors_origins`` collapses
     to the single built-in ``DEFAULT_CORS_ORIGIN`` (``https://app.fauna.
     social`` — ``node_policy_core.rs:124,186``), which a random local test
     port never matches. Routing the WS-RPC connection through that CORS gate
     would confound the differential: a rejected Origin would ALSO read as
     "never Connects", for a reason having nothing to do with the cert trust
     model this track exists to pin. So the web leg must navigate a real
     browser straight to the box's own ``https://.../app/`` — same origin,
     no proxy, exactly the real bug's topology.
  2. **That means the DOCKER IMAGE's ``/usr/share/fauna-web`` must be a
     current build.** Verified 2026-07-29: the locally retagged
     ``fauna-nest-test:local`` serves the pre-`df0e2
     eda83` info-page placeholder at ``/app/`` (probed directly — see
     ``_app_route_serves_current_spa`` below), and even a `static_dir`-fixed
     image built before that fix would ship a bundle that predates
     ``ConnectionState::Unreachable`` and would just hang on "Connecting…"
     forever instead of settling on "Cannot connect". **So the web half (and
     the Slice-2 browser "last inch" leg in the same TODO) needs a FRESH
     ``build-nest-image.yml --ref main`` dispatch** — the same one is already waiting on; bundle
     them. This corrects the prior disposition text's "needs no new image"
     claim, which held for the native half only.

``_app_route_serves_current_spa`` is kept here (unused by any test yet) so the
web leg's fixture-level gate is a two-line addition once an image exists,
without re-deriving the detection logic.
"""

from __future__ import annotations

import subprocess
import time

import pytest

from helpers.app_surface import declared_absence, skip_unbuilt

from .helpers import (
    claim_admin_api,
    docker_build,
    find_free_port,
    get_repo_root,
    remove_container,
    start_container,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"), pytest.mark.tier_4]

CLAIM_CODE = "TRUST1"
# Load-robust wait ceilings (never tighten — testing.md § point 14). TOFU
# accept-and-connect is fast (no retry needed on a first contact); size it
# like the other tier_4 admin-claim waits. CONNECT_WAIT_S intentionally does
# NOT need to cover the 8-consecutive-failure Unreachable path — that's the
# (currently unwritten) web leg's problem, not this one's.
CONNECT_WAIT_S = 60.0


@pytest.fixture()
def self_signed_floor_nest(app):
    """A freshly claimed nest that never gets a real certificate.

    The bare ``handle="admin"`` claim (no ``mail_domain``) keeps the
    ``localhost`` identity — there is no real domain to ever order a
    certificate for, so ACME never even attempts and the box parks on the
    always-live self-signed floor forever (``docker/entrypoint.sh``'s
    unconditional floor write). Exactly the precondition
    ``test_mail_client_ui_enable_docker.py`` already proves a linux driver
    can claim + connect to over real TLS.
    """
    # ── The app gate comes FIRST, before the image is touched ──────────────
    # It used to sit in the test body, one fixture too late: `_image` was a
    # module-scoped fixture, and pytest instantiates higher-scoped fixtures
    # ahead of function-scoped ones whatever the argument order, so no
    # fixture-ordering trick could have saved it either. A non-linux run
    # therefore paid for a full image build AND a container start before
    # skipping. Both sibling files in this directory already put their gate
    # first for exactly this reason ("the skip is the FIRST thing so the heavy
    # image build + container start never run"), and calling `docker_build`
    # inline below is how they do it — it is module-cached, a no-op re-check
    # once the tag is fresh, so the retired fixture bought nothing.
    #
    # Web is not unbuilt debt here, and calling it that would be a standing
    # invitation to "close the gap" — there is no gap to close. This test is
    # the NATIVE half by construction (see the module docstring): it asserts a
    # client reaches ``Connected`` **because** it can TOFU. A browser
    # permanently cannot, and the owner doc says so in as many words — "the
    # difference is structural, not a gap to close later: a browser exposes no
    # received certificate to WASM, so there is no SPKI to compare the
    # signature against and Axis 1's channel binding cannot be completed."
    # Web's own leg is the OPPOSITE assertion (the box it cannot talk to), a
    # separate image-gated test the module docstring already scopes — declaring
    # the absence here does not stand in for it.
    if app.driver.is_web():
        declared_absence(
            app.driver,
            capability="TOFU first-contact trust — a browser exposes no "
            "received certificate to WASM, so Axis 1's channel binding "
            "cannot be completed and web is strictly weaker than native "
            "by construction",
            doc="docs/goal/architecture/security.md § Transport trust — "
            "authenticating a nest's TLS without a public CA (the "
            "first-connect table's self-hosted-pre-claim row)",
        )

    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the native/TOFU trust-differential drive",
            detail="proven on linux; the remaining NATIVE apps (tui, windows, "
            "macos, ios) are ordinary cross-app trickle-down — they share "
            "trust.rs's TOFU model, so each one's leg is a driver swap, not "
            "new trust work. Every leg needs a fresh nest image on a "
            "docker-capable box, which is the actual gate here",
            tracked="",
        )

    # `FAUNA_REUSE_IMAGE=1` + a pre-pulled `fauna-nest-test:local` is the
    # sanctioned path (no local nest-image builds on a dev VM); this call is a
    # no-op re-check when that tag already exists, matching every sibling
    # fixture in this directory.
    docker_build(get_repo_root())

    port = find_free_port()
    name = f"fauna-nest-trust-diff-{port}"
    start_container(name, port, env={"FAUNA_CLAIM_CODE": CLAIM_CODE})
    try:
        wait_for_health(port, name)
        admin = claim_admin_api(port, CLAIM_CODE, handle="admin")
        # Hand-built https nest — not routed through `_as_nest_handle`, so this
        # port must self-register: `_relaunch_trusting_nest`'s nest.info read
        # (and every other port-keyed `common.auth` dial) would otherwise speak
        # plain http/ws to a TLS-only listener and raise.
        from common.auth import mark_tls_nest

        mark_tls_nest(port)
        yield {"name": name, "port": port, "url": f"https://127.0.0.1:{port}", "admin": admin}
    finally:
        remove_container(name)


def _app_route_serves_current_spa(url: str) -> bool:
    """True once the box's own ``/app/`` serves the real SPA rather than the
    static info-page placeholder. Kept for the (unwritten) web leg's
    fixture-level gate — see the module docstring § Web half.

    A pre-fix image fails this outright (placeholder text
    present); an image with that fix but built before the later bundle fix passes
    this check while still shipping a bundle with no ``ConnectionState::
    Unreachable`` — that residual gap is why the web leg additionally needs
    the image to postdate the connection-state work, not just this probe.
    """
    import ssl
    import urllib.request

    ctx = ssl._create_unverified_context()
    try:
        resp = urllib.request.urlopen(f"{url}/app/", timeout=10, context=ctx)
        body = resp.read(4000).decode("utf-8", "replace")
    except Exception:
        return False
    return "Fauna Nest API" not in body


def _connection_status(app) -> str | None:
    """The global ``connection-status`` indicator text, or ``None`` where the
    element isn't implemented on this client (mirrors
    ``test_nest_flip_resilience.py``'s helper of the same name — kept local
    here since there is no shared home for it yet)."""
    if app.is_visible("connection-status"):
        return app.get_text("connection-status")
    return None


def _wait_for_status(app, predicate, timeout: float) -> str | None:
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = _connection_status(app)
        if predicate(last):
            return last
        time.sleep(0.5)
    return last


@pytest.mark.feature("connect-and-sign-in")
def test_native_tofu_client_reaches_connected_and_admin_on_self_signed_floor(app, self_signed_floor_nest):
    """native half.

    A native/TOFU client reaches ``Connected`` and passes ``am-i-admin``
    against a box that only ever serves its self-signed floor — pinning
    ``trust.rs``'s TOFU model as the reason a native app survives exactly
    the box the (unwritten) web/WebPKI leg cannot talk to.
    """
    nest = self_signed_floor_nest

    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)

    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": nest["admin"]["secret_hex"],
        },
        "nav": {"stack": [{"view": "feed"}]},
    })

    status = _wait_for_status(app, lambda s: s == "Connected", CONNECT_WAIT_S)
    assert status == "Connected", (
        f"native TOFU client never reached Connected against the self-signed "
        f"floor (last connection-status={status!r}) — trust.rs should accept "
        f"an unpinned first-contact cert unconditionally, so this should "
        f"settle on Connected quickly, not sit on Connecting/Unreachable"
    )
    assert app.is_visible("admin-tab"), (
        "am-i-admin should pass for the claimed admin once Connected — "
        "admin-tab is fail-closed on any transport/auth failure "
        "(apps/fauna-web/src/routes/+layout.svelte's checkIsAdmin, mirrored "
        "natively), so its absence here would mean the WS connection isn't "
        "really usable even though the indicator reads Connected"
    )
