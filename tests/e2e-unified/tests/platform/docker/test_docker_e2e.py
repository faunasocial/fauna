"""E2E test: the Docker image's bundled web app runs, against the real image.

Browser variant — the **bundled** web app at ``/app/`` actually runs: identity
generation (wasm keygen in-image) and cross-stage navigation, against the real
image. Its API sibling — claiming admin over the real wire, setup-status never
leaking the code — lives in ``test_docker_api_claim.py``: it drives no app
driver, so it is a ``[nest]`` witness and must not sit in a module whose
``browser_session`` import makes the catalog's driver-closure scan read it as
app-driving (split 2026-08-30).

The container boots **domainless** (the box learns its identity from the
admin's claim handle, not a boot env), with a known claim code injected via
``FAUNA_CLAIM_CODE``.

Mail-bridge enable + s6 supervision is **not** re-tested here — that is the
canonical job of ``test_mail_client_ui_enable_docker.py`` (the real
``set_mail_enabled`` → ``/data/imap-enabled`` → ``fauna-mail-bridge-{mta,mda}``
path) and ``test_mail_deploy_lifecycle.py`` (bring-to-serving). The legacy
"enable the generic bridge service → s6 ``fauna-bridge`` comes up" model these
tests used to assert was removed in the I6 mail cutover.
"""

import subprocess
import time

import pytest

from helpers import budgets

from .helpers import (
    browser_session,
    docker_build,
    find_free_port,
    generate_claim_code,
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

# `web` scopes these web-SPA-driven docker tests to --client web (they were
# wrongly "client-independent", so a non-web --include-independent run pulled
# them in); the marker is a no-op for the no-`--client` `just e2e-tier-4-test`.
pytestmark = [pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"), pytest.mark.tier_4, pytest.mark.web]


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the Docker image once per module."""
    repo = get_repo_root()
    tag = docker_build(repo)
    yield tag


@pytest.fixture()
def nest(docker_image):
    """Start a fresh container with a known claim code.

    Yields dict with port, claim_code, container_name, nest_url.
    Cleans up the container after the test.
    """
    port = find_free_port()
    claim_code = generate_claim_code()
    name = f"fauna-nest-e2e-{port}"
    start_container(name, port, env={
        "FAUNA_MODE": "public",
        "FAUNA_CLAIM_CODE": claim_code,
    })
    try:
        wait_for_health(port, name)
        yield {
            "port": port,
            "claim_code": claim_code,
            "container_name": name,
            "nest_url": f"https://127.0.0.1:{port}",
        }
    finally:
        remove_container(name)


# ── Test A: Browser onboarding ────────────────────────────────────────


@pytest.mark.feature("get-the-app")
def test_docker_browser_onboarding(nest):
    """The bundled SPA boots, runs its wasm runtime, and is interactive in the
    real deploy image.

    This is the one browser smoke at tier_4. Its unique value — which no other
    tier catches — is that the SPA **as packaged into the deploy image** (served
    at ``/app/`` over the self-signed bootstrap cert) actually loads and executes
    in a browser: a misbundled SPA (wrong base path, missing chunk, broken wasm
    glue) passes every API test but renders blank for users. We drive the
    identity step — ``create-identity-button`` → ``secret-key-display`` — because
    rendering the freshly generated Ed25519 secret proves the bundled **wasm
    keygen actually ran in-image**, then advance to ``handle_entry`` to prove
    cross-stage navigation works.

    We deliberately stop before the claim: the full handle-first claim flow is
    covered at tier_3 (``test_onboarding_localhost.py``,
    ``test_claim_code_unclaimed_nest.py``), and claiming *via the localhost
    handle* can't reach this image anyway —
    ``fauna_provisioning::probe::resolve_handle_domain`` resolves a loopback host
    to ``http://localhost:3000`` (loopback ⇒ http, default port), which doesn't
    match the domainless image serving https via its always-live self-signed
    floor. Claiming over the real wire in the image is instead proven by
    ``test_docker_api_provisioning``.
    (Whether onboarding a ``localhost``-domain *deploy* via the ``localhost``
    handle is a real product gap or just an artifact of the tests' ``localhost``
    domain is flagged for follow-up in NEXT — it is not a packaging concern.)

    ``browser_session`` holds the machine-wide snap-Chromium flock (shared with
    the web driver) so this serializes against any concurrent ``--client web``
    run instead of crashing on the singleton.
    """
    from playwright.sync_api import expect

    url = nest["nest_url"]

    with browser_session() as page:
        # Convention 6: a blank or stuck SPA says WHY in the failure — the
        # console errors and failed requests are what a misbundle leaves behind.
        console_errors: list[str] = []
        failed_requests: list[str] = []
        page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)
        page.on("pageerror", lambda e: console_errors.append(f"pageerror: {e}"))
        page.on("requestfailed", lambda r: failed_requests.append(f"{r.url} ({r.failure})"))
        page.on("response", lambda r: failed_requests.append(f"{r.url} → {r.status}") if r.status >= 400 else None)

        # The bundled SPA is served by the image over the self-signed cert.
        # `domcontentloaded`, not `networkidle`: the SPA keeps sockets open, so
        # "idle" is a timing fact; the testid waits below are the state waits.
        page.goto(f"{url}/app/onboarding", wait_until="domcontentloaded",
                  timeout=int(budgets.SPA_FIRST_PAINT_S * 1000))

        # A shipped build seals the identity store, so a fresh browser lands on
        # the unlock page's CREATE mode before onboarding (apps/web.md § Identity store
        # at rest — unlock comes before launch routing). Setting the
        # passphrase also proves the bundled wasm seal chunk ran in-image.
        try:
            page.locator('[data-testid="unlock-confirm-input"]').wait_for(
                state="visible", timeout=int(budgets.SPA_FIRST_PAINT_S * 1000)
            )
        except Exception as e:
            pytest.fail(
                f"the unlock page's confirm input never became visible on {page.url} "
                f"({type(e).__name__}); console errors={console_errors[:10]}; "
                f"failed requests={failed_requests[:10]}"
            )
        page.fill('[data-testid="unlock-passphrase-input"]', "docker smoke passphrase")
        page.fill('[data-testid="unlock-confirm-input"]', "docker smoke passphrase")
        page.click('[data-testid="unlock-submit-button"]')

        # Identity: create a fresh Ed25519 identity. The secret rendering proves
        # the bundled wasm keygen executed inside the image.
        create = page.locator('[data-testid="create-identity-button"]')
        try:
            create.wait_for(state="visible", timeout=int(budgets.SPA_FIRST_PAINT_S * 1000))
        except Exception:
            painted = page.eval_on_selector_all(
                "[data-testid]", "els => els.map(e => e.getAttribute('data-testid'))"
            )
            error = page.locator('[data-testid="error-message"]')
            error_text = error.first.inner_text() if error.count() else "<none>"
            pytest.fail(
                "the image's SPA never painted create-identity-button on "
                f"{page.url}: error-message={error_text!r}; painted testids={painted[:40]}; "
                f"body={page.inner_text('body')[:400]!r}; console errors={console_errors[:10]}; "
                f"failed requests={failed_requests[:10]}"
            )
        create.click()
        expect(page.locator('[data-testid="secret-key-display"]')).to_be_visible(
            timeout=int(budgets.UI_SETTLE_S * 1000)
        )
        page.click('[data-testid="identity-continue-button"]')

        # The recovery kit follows the identity step (onboarding.md § 1
        # Identity); skipping it is one click and never blocks onboarding.
        page.click('[data-testid="recovery-kit-skip-button"]')

        # Cross-stage navigation works: the wizard advances to handle entry.
        expect(page.locator('[data-testid="handle-input"]')).to_be_visible(
            timeout=int(budgets.UI_SETTLE_S * 1000)
        )
