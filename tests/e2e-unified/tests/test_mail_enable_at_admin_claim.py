"""tier_3 (local CI): the **admin claim + mail-enable** round-trip against a
fresh unclaimed nest, covering TWO behaviors the no-modes retirement (ratified
2026-07-12) split apart.

Historically this file drove a single flow: a fresh admin claims an
*unclaimed* nest through the believable onboarding wizard, ticks the
`onboarding-enable-email-checkbox` ON, and the post-login launch glue
auto-mints the admin's own mailbox **and** enables deployment-wide mail with
no further UI action. That checkbox — and the intervening
`encryption_mode_choice` page it lived on — is now RETIRED WITHOUT relocation
(`docs/goal/behavior/onboarding.md` § 3b): claim lands the wizard directly on
`nat_mode_choice`, and mail/CalDAV/CardDAV/WebDAV enablement is a
MACHINE-DERIVED default — ON iff the handle targets a real registerable
domain, OFF for a loopback/IP target — applied by the post-claim launch glue
with **no onboarding-time override**.

Two of the three tests here claim with a LOOPBACK handle
(`<localpart>@127.0.0.1:<port>`), for which the derived default is
unconditionally OFF — and there is no client-reachable way to force it ON at
claim time any more (faking it via a raw RPC would test something no real user
can do — see the e2e "drive mutations through the UI" rule). The **third**
drives the derived-ON branch for real, with a domain-shaped handle; until
2026-08-13 that was unreachable locally on every app, because the
authenticated session established after `LoggedIn` dialed the literal
`https://{domain}` no DNS resolves (the pre-identity calls already redirected
through `provider_base_urls["nest"]`; the authed dial had no seam). The
`resolved_nest_dial_url` seam closed that. So this file proves three separate,
real things:

  1. **The derived default holds**: claiming with a loopback handle does NOT
     auto-enable deployment mail (`test_admin_claim_with_loopback_handle_
     derives_mail_disabled`) — the correct, spec'd outcome for a non-real-domain
     target, and worth pinning now that it's reachable no other way.
  2. **The admin can still enable mail afterward, through the client UI**
     (`test_admin_enables_mail_via_mail_settings_after_loopback_claim`): the
     admin visits Settings → Mail and clicks enable. `libs/fauna-client-
     mail-settings/src/machine.rs::enable_mail` — the SAME function the
     retired glue's `enable_mail_with_generated_password` wrapped (its own doc
     comment: "wraps the same `enable_mail` path as the manual mail-settings
     enable") — mints the admin's MSEK + wrapped-MSEK + recipient pubkey +
     submission token + credential, and fires the deployment-wide
     `set_mail_enabled(true)` as its final step (Admin-class on the nest, so
     it takes effect for the admin's own enable). So driving the real
     mail-settings UI reproduces the exact mint-then-flag sequence the old
     automatic glue used to run — just admin-initiated instead of automatic.
  3. **The derived default's ON branch fires, with no user action after the
     claim** (`test_admin_claim_with_real_domain_handle_auto_enables_mail`,
     tui): a domain-shaped handle derives ON, and the post-`LoggedIn` launch
     glue mints the admin mailbox and flips deployment mail, CalDAV, CardDAV
     and WebDAV — all four of § 3b's intents, not mail alone — by itself. This
     is the branch `onboarding.md` § 3b actually specifies for the common
     deployment, and it had never been proven anywhere but the live-remote
     test below.
  4. **The CLAIM AXIS gates all of it: a plain SIGN-IN enables nothing**
     (`test_a_returning_admin_sign_in_issues_no_deployment_enable`, tui).
     § 3b's two defaulting axes say how the intents default *once a claim has
     happened* — never whether one did. Three routes reach the single
     `WizardOutcome::LoggedIn` the launch glue reads those getters at (admin
     claim, `AlreadyOnNest` sign-in, invite redeem), and only the claim routes
     may request enablement. Absent that gate a returning admin merely signing
     in on a real-domain public box fired four Admin-class deployment writes
     against a box they had already configured — observed against the live box
     2026-08-16, fixed by the `claim_completed` conjunct in the shared machine.
     Test 3 and test 4 are the same journey differing in exactly one step —
     same box, same identity, same handle, same NAT answer — so together they
     say the axis is the claim and nothing else.

Why this file exists (no existing harness covers it locally):
  - `test_mail_enable_live_nest.py` is the admin round-trip, but live-remote-only
    (factory-resets the live nest, env-gated) — it never runs in local CI.
  - `test_mail_auto_enable_first_setup.py` proves the *non-admin* auto-enable
    policy branch (`auto_enable_mail_for_new_users`), an unrelated axis from
    the admin's own claim + mail-settings-enable flow this file covers.

Flow (everything a real admin does, in order):
  1. A fresh identity is imported into the believable wizard (no pre-registration —
     the claim binds it as admin).
  2. The loopback handle `<localpart>@127.0.0.1:<port>` resolves to the local nest
     (`resolve_handle_domain → is_local → skip DNS`); the handle check finds the
     fresh nest unclaimed → `UnregisteredUnclaimedNest` → Continue → `claim_code`.
  3. The admin submits `CLAIM_CODE` → `fauna.auth.claim_admin` → `nat_mode_choice`
     (dismissed, confirm-only) → `WizardOutcome::LoggedIn`.
  4. (Test 1) Deployment mail stays OFF — the loopback handle derives no
     auto-enable. (Test 2) The admin visits Settings → Mail and enables it:
     deployment mail flips ON and the admin's `Default` credential appears.

No IMAP: unlike the non-admin first-setup test, this asserts only the **mint**
(credential row) + the **deployment mail-enabled** flag — not an end-to-end IMAP
round-trip (that is the non-admin test's and the live test's job). So no MDA bridge
and no routable domain are needed (`fauna.test` mints + enables fine without DNS;
the enable-time `ensure_primary_mail_domain` safety net is a no-op on a local domain
but `set_mail_enabled` + the credential mint succeed regardless).

Desktop apps (linux + macOS + tui): all wire the same mail-settings enable
path (linux's mail-settings action / shared FaunaKit mail settings / tui's
`Op::SubmitMailCredential`). On macOS this runs through the in-process driver
(no XCUITest). iOS uses the same FaunaKit glue but is XCUITest-gated — run
there once the in-process iOS driver lands.
"""

from __future__ import annotations

import secrets
import sys
import time
from pathlib import Path

import pytest

# Mirror the sys.path bootstrap of the other local-onboarding e2e files so the
# shared `common` package (tests/common/) and the e2e-unified helpers resolve.
_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)
_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from clients.ws_rpc_admin_client import WsRpcAdminClient  # noqa: E402
from clients.ws_rpc_anon_client import WsRpcAnonClient  # noqa: E402
from common.nest import CLAIM_CODE  # noqa: E402
from conftest import MAIL_PRIMARY_DOMAIN, get_available_apps  # noqa: E402
from helpers.app_surface import skip_unbuilt  # noqa: E402
from helpers.authenticated_shell import SHELL_MARKERS  # noqa: E402
from helpers.waiting import wait_until  # noqa: E402

_avail_clients = get_available_apps()
if not any(c in _avail_clients for c in ("linux", "macos", "ios", "tui")):
    pytest.skip(
        "drives the linux/macOS/iOS/tui client admin-claim onboarding UI",
        allow_module_level=True,
    )

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


# The authed app shell has mounted once any main-view landmark is visible: this
# test only needs "onboarding is done" before it navigates to mail-settings, and
# apps legitimately differ on where they land. The set is shared (which app lands
# where is documented there) — a private copy is what traced two 120s windows hangs to.
_LOGGED_IN_MARKERS = SHELL_MARKERS

# ── The post-claim glue's causal anchor — convention 14 ────────────────────────
#
# Every "what did the post-claim glue do" assertion here (the stays-OFF cases
# included) anchors on `SERVING_ENABLEMENT_KEY`, never on a settle window:
# `helpers.waiting.await_serving_enablement_for` returns once the run for the
# identity this test onboarded is `completed`, i.e. every step the glue planned
# has answered (the decide-nothing run included). From then on the deployment
# toggles are final for this onboarding, so ONE read of them is a sound verdict
# in both directions: a slow enable cannot miss it and a late wrong enable
# cannot slip past it. `_CONTROL_ENABLE_WAIT_S` only bounds the claim leg's
# positive wait for its enable to land.
_CONTROL_ENABLE_WAIT_S = 120.0


# ── Fixture: a fresh UNCLAIMED nest, mail NOT pre-enabled ──────────────────────
#
# The nest is genuinely unclaimed (the UI drives `fauna.auth.claim_admin`), no
# local mail domain is pre-registered, and `set_mail_enabled` is NOT pre-fired
# — both tests in this file assert what happens to mail-enablement from that
# clean starting point. No MDA bridge — this asserts the mint + the deployment
# flag, not an IMAP round-trip.


@pytest.fixture
def unclaimed_mail_nest(request, nest_mode, tmp_path_factory):
    """A fresh **unclaimed** nest whose `handle_domain == MAIL_PRIMARY_DOMAIN`
    (`fauna.test`). The admin claims it through the onboarding UI with a
    loopback handle (derives mail OFF); one test pins that, the other then
    enables mail through the mail-settings UI. Nothing mail-related is
    pre-provisioned.

    Serves REAL self-signed HTTPS (`serve_tls=True`): after Pillar C (uniform
    https) the loopback `…@127.0.0.1:{port}` handle resolves to `https://…`, so
    the production-faithful posture is a nest that actually serves TLS there — the
    native app trusts the self-signed floor via channel-binding (the same
    `AcceptProvisional` path `test_onboarding_self_signed_probe.py` proves), and
    the **post-LoggedIn** authed connection reuses the onboarding-graduated SPKI
    pin (`ws_adapter.rs`) to reach the same https nest. No `set_provider_base_urls`
    override is needed — the resolved https URL points straight at the nest."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "admin-claim-mail-nest",
        handle_domain_seed=MAIL_PRIMARY_DOMAIN, unclaimed=True, serve_tls=True,
    )
    assert nest["admin"] is None, "fixture must hand the UI a genuinely unclaimed nest"
    try:
        yield nest
    finally:
        cleanup()


# ── Helpers ───────────────────────────────────────────────────────────────────


def _drive_admin_claim_to_logged_in(
    app, nest, secret_hex: str, typed_handle: str, *, nat_mode: str | None = None
) -> None:
    """Drive the believable wizard (linux or macOS in-process) for a fresh admin
    claiming an unclaimed nest, from identity import to the logged-in feed.

    `nat_mode` selects a radio on `nat_mode_choice` before confirming
    (`"public"` / `"private"`); the default confirms the pre-selected seed,
    which is the common case every caller but the private-box pin wants. It is
    a parameter because § 3b's second axis is answered *here* — the derived
    enablement a caller then reads is a consequence of this click, so a caller
    that wants the private branch must be able to ask for it without copying
    the whole drive.

    Platform-agnostic: `app.onboarding.*` + the claim IDs are the shared
    ui.yaml IDs on both desktop apps. The handle is loopback (for local
    nest discovery); the canonical handle binds to `<localpart>@<handle_domain>`.

    No-modes retirement (ratified 2026-07-12): claim now lands the wizard
    directly on `nat_mode_choice` (the terminal admin-path step; the former
    intervening `encryption_mode_choice` page — and its enable-email checkbox
    — is retired without relocation). Dismissed via `finish_nat_mode()`
    (confirm-only common case), the same helper every other e2e claim flow
    uses.
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)  # fresh identity → handle_entry
    ob.fill_handle(typed_handle)
    # Pillar C (uniform https): the loopback `…@127.0.0.1:{port}` handle resolves to
    # `https://127.0.0.1:{port}` and the nest serves real self-signed HTTPS there
    # (`serve_tls=True`), so NO `set_provider_base_urls` override is needed — the
    # probe (and the post-LoggedIn authed connection) reach the nest directly and
    # trust the self-signed floor via channel-binding / the graduated SPKI pin.
    ob.run_handle_check(timeout=45)  # local nest probe → UnregisteredUnclaimedNest
    ob.submit_handle()

    # claim_code page — submit the one-time code (drives `fauna.auth.claim_admin`).
    app.driver.wait_for("claim-code-input", timeout=45)
    app.driver.clear_and_type("claim-code-input", CLAIM_CODE)
    app.driver.click("claim-code-submit-button")

    # A successful claim lands the wizard on nat_mode_choice; dismiss it
    # (confirm-only — accepts the pre-selected seed) to exit to LoggedIn. The
    # post-claim launch may also surface a launch/retry screen; click through
    # whatever appears until the authed app renders.
    deadline = time.monotonic() + 120.0
    while time.monotonic() < deadline:
        if any(app.driver.is_visible(m) for m in _LOGGED_IN_MARKERS):
            return
        try:
            app.onboarding.finish_nat_mode(mode=nat_mode)
        except Exception:
            pass
        for btn in ("launch-retry-button",):
            try:
                if app.driver.is_visible(btn):
                    app.driver.click(btn)
            except Exception:
                pass
        time.sleep(2.0)

    # Diagnostic failure — dump what's on screen.
    err = ""
    for eid in ("launch-transient-error", "error-message"):
        try:
            if app.driver.is_visible(eid):
                err = app.driver.get_text(eid)
                break
        except Exception:
            pass
    try:
        tree = app.driver.tree()
    except Exception as e:  # pragma: no cover - diagnostic only
        tree = f"(tree dump failed: {e})"
    pytest.fail(
        "admin never reached the authed app after the claim + nat_mode_choice "
        f"onboarding. launch error: {err!r}\n--- accessibility tree ---\n{tree}"
    )


def _drive_sign_in_to_logged_in(app, secret_hex: str, typed_handle: str) -> None:
    """Drive the wizard for a RETURNING admin — same identity, same handle, on a
    box that identity already claimed — from identity import to the logged-in
    shell.

    Identical to `_drive_admin_claim_to_logged_in` up to `submit_handle()`, and
    that is the whole point: the two journeys differ in exactly one step. Here
    the handle check's silent challenge finds the identity already registered
    (`AlreadyOnNest`) and Continue exits STRAIGHT to the authenticated app —
    no claim-code page, no `nat_mode_choice` (ui.yaml's handle_entry: "On
    AlreadyOnNest, Continue exits straight to the authenticated app").

    Reaching `claim-code-input` here is a hard failure, not a branch to click
    through: it would mean the nest is unclaimed, so the leg under test never
    happened.
    """
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)  # → handle_entry
    ob.fill_handle(typed_handle)
    ob.run_handle_check(timeout=45)  # local nest probe → AlreadyOnNest
    ob.submit_handle()

    def _landed() -> bool:
        if any(app.driver.is_visible(m) for m in _LOGGED_IN_MARKERS):
            return True
        if app.driver.is_visible("claim-code-input"):
            pytest.fail(
                "the sign-in leg reached the claim-code page: the handle check "
                "did not answer AlreadyOnNest, so this nest is not claimed by "
                "this identity and the returning-admin journey never ran"
            )
        for btn in ("launch-retry-button",):
            try:
                if app.driver.is_visible(btn):
                    app.driver.click(btn)
            except Exception:
                pass
        return False

    # Shared deadline poll (convention 14 / `helpers.waiting`), not a bare sleep
    # loop: a green run returns the instant the shell renders.
    try:
        wait_until(_landed, 120.0, interval=2.0)
        return
    except AssertionError:
        pass

    err = ""
    for eid in ("launch-transient-error", "error-message"):
        try:
            if app.driver.is_visible(eid):
                err = app.driver.get_text(eid)
                break
        except Exception:
            pass
    try:
        tree = app.driver.tree()
    except Exception as e:  # pragma: no cover - diagnostic only
        tree = f"(tree dump failed: {e})"
    pytest.fail(
        "the returning admin never reached the authed app after signing in. "
        f"launch error: {err!r}\n--- accessibility tree ---\n{tree}"
    )


def _wait_for_auto_minted_credential(app, timeout: float = 60.0) -> bool:
    """Wait for the auto-minted `Default` credential to surface on mail-settings,
    re-opening the page each iteration.

    The auto-mint is a fire-and-forget background task that completes ~1-2s after
    the feed renders. The desktop settings sub-stack re-hydrates the mail page only
    when it is *shown*, so a single show right after onboarding can race ahead of
    the mint — re-show the page (away → back) until the credential appears (mirrors
    a real admin opening mail-settings to read their generated password)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        # Show a different settings sub-page (unmaps mail), then re-show mail.
        app.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]},
        })
        app.mail_settings.navigate()
        if app.mail_settings.wait_for_credential_count_at_least(1, timeout=6.0):
            return True
    return False


def _wait_deployment_mail_enabled(nest_url: str, timeout: float = 60.0) -> bool:
    """Poll the anonymous `fauna.setup.status` until `email_enabled` flips true.

    `set_mail_enabled(true)` is the final step of `enable_mail` (whichever
    caller reaches it — the mail-settings UI or the retired auto-glue), so a
    true reading also implies the admin mailbox already minted."""
    deadline = time.monotonic() + timeout
    last = None
    with WsRpcAnonClient(nest_url) as anon:
        while time.monotonic() < deadline:
            last = anon.call("fauna.setup.status", {})
            if last.get("email_enabled"):
                return True
            time.sleep(2.0)
    return False


def _wait_all_deployment_enables(
    admin_secret_hex: str, nest_url: str, timeout: float = 60.0, *, axes=None
) -> dict[str, bool]:
    """Poll `_read_deployment_enables` until all four axes (or just `axes`) read
    true, via the shared `wait_until` deadline poll (convention 14), or return the
    last reading once `timeout` elapses (never raises — the caller asserts on the
    returned dict, which always carries all four, so a partial result still
    self-diagnoses which axis lagged)."""
    last: dict[str, bool] = {}

    def _all_true() -> bool:
        nonlocal last
        last = _read_deployment_enables(admin_secret_hex, nest_url)
        return all(last[a] for a in (axes or last))

    try:
        wait_until(_all_true, timeout, interval=2.0)
    except AssertionError:
        pass
    return last


def _admin_keys(admin_secret_hex: str):
    """The ``(actor_id, signing_key)`` byte pair an Admin WS-RPC client needs,
    from a test-held admin secret. Twin of `helpers.caldav_onboarding._admin_keys`
    (kept local rather than importing a sibling's private name)."""
    from nacl.signing import SigningKey

    sk = SigningKey(bytes.fromhex(admin_secret_hex))
    return bytes(sk.verify_key), bytes(sk)


def _read_deployment_enables(admin_secret_hex: str, nest_url: str) -> dict[str, bool]:
    """The four deployment-wide serving toggles § 3b's launch glue can write,
    read in ONE Admin-class call (`fauna.bridges.get_mail_config` → the overlaid
    effective `FetchConfigReply`).

    All four, not just the two tui's glue fires today: linux fires four
    (`set_mail_enabled` via `provision_mail_at_first_setup`, plus
    `set_{caldav,carddav,webdav}_enabled`), and reading the full set means this
    pin keeps covering the whole surface if tui's glue grows the other two.
    Note `carddav_enabled`/`webdav_enabled` FALL BACK to `mail_enabled` when
    never explicitly set, so an accidental mail-enable shows up on all four —
    which only sharpens the assertion."""
    actor_id, signing_key = _admin_keys(admin_secret_hex)
    with WsRpcAdminClient(nest_url, actor_id=actor_id, signing_key=signing_key) as ws:
        cfg = ws.call("fauna.bridges.get_mail_config", {})
    return {
        axis: bool(cfg.get(axis))
        for axis in ("mail_enabled", "caldav_enabled", "carddav_enabled", "webdav_enabled")
    }


def _disable_deployment_serving(admin_secret_hex: str, nest_url: str) -> None:
    """Put the box back to "already configured, serving currently OFF" — the
    starting shape of the live-box journey this module's test 4 reproduces.

    A raw Admin RPC rather than the settings UI on purpose: this is FIXTURE
    SETUP for the leg under test, not the behavior under test (the e2e
    "drive mutations through the UI" rule's stated carve-out), and the UI path
    for exactly these toggles is already covered by
    `test_admin_enables_mail_via_mail_settings_after_loopback_claim` above and
    by `helpers.caldav_onboarding._ensure_enablement`."""
    actor_id, signing_key = _admin_keys(admin_secret_hex)
    with WsRpcAdminClient(nest_url, actor_id=actor_id, signing_key=signing_key) as ws:
        for kind in (
            "fauna.bridges.set_mail_enabled",
            "fauna.bridges.set_caldav_enabled",
            "fauna.bridges.set_carddav_enabled",
            "fauna.bridges.set_webdav_enabled",
        ):
            ws.call(kind, {"enabled": False})


def _glue_verdict(
    app, admin_secret_hex: str, nest_url: str, *, prior_runs: int = 0
) -> tuple[dict, dict[str, bool]]:
    """Wait for the post-claim serving-enablement run of this admin to finish,
    then read the four deployment toggles ONCE.

    Returns ``(decided, enables)``: what the app's glue decided (the anchor's
    own record) and what the nest holds afterwards. The anchor replaces the
    fixed settle windows the "stays OFF" cases used to sleep out: the step is
    the only writer of these toggles between the claim and this read (no UI
    action intervenes), so once its run is `completed` a single read also
    answers "was one EVER enabled" — an enable that fired and was undone would
    need a second writer."""
    from nacl.signing import SigningKey

    from helpers.waiting import await_serving_enablement_for

    actor_id_hex = bytes(SigningKey(bytes.fromhex(admin_secret_hex)).verify_key).hex()
    decided = await_serving_enablement_for(
        app.driver, actor_id_hex, prior_runs=prior_runs
    )
    return decided, _read_deployment_enables(admin_secret_hex, nest_url)


# ── The real-domain (derived-ON) fixture ──────────────────────────────────────


@pytest.fixture
def unclaimed_real_domain_nest(request, app, nest_mode, tmp_path_factory):
    """A fresh **unclaimed** nest served over plain HTTP on loopback, reached by
    the wizard through the `provider_base_urls["nest"]` override — which this
    fixture both installs and, crucially, **clears on teardown**.

    The loopback fixture above cannot exercise the derived-**ON** branch: its
    typed handle is an IP literal, which `handle_targets_real_domain` classifies
    as non-real by design. Driving the ON branch needs a **domain-shaped** typed
    handle (`<localpart>@fauna.test`) that no DNS resolves — so every nest call
    rides the `"nest"` override instead, which is why this fixture serves plain
    HTTP rather than the sibling's self-signed TLS: the override replaces the
    whole URL (scheme, host and port), so there is no `https://fauna.test` to
    handshake against.

    ⚠ **The clear is not tidiness — it is what keeps this module's other tests
    honest.** The `app` fixture resets between tests but does NOT relaunch (the
    cold relaunch is a per-*module* boundary), and the override lives in the
    long-lived `OnboardingMachine`. Leaving it installed pointed every later
    test's handle-check at this nest's port — dead by then, since the fixture
    tears it down — and the wizard's Continue button stayed disabled with no
    hint as to why. Installing and clearing it here means that holds however the
    test exits, including a failure.

    The app gate lives here rather than in the test body (the `conftest.py`
    `SuccessionWitness` fixture is the precedent) so an app that cannot use the
    seam never pays for a nest start and an override install it would only skip
    past — and never risks carrying that override into the next test."""
    if not (
        app.driver.is_tui() or app.driver.is_linux()
        or app.driver.is_macos() or app.driver.is_ios()
    ):
        # web ALSO already resolves the dial (its own TS twin `nodeUrl()`,
        # `apps/fauna-web/src/lib/api.ts`, wired end-to-end: the driver's
        # `set_provider_base_urls` query-param reload reaches
        # `onboarding/machine.svelte.ts`'s init, which calls
        # `setNestDialOverride` alongside the onboarding-provider override —
        # landed 2026-08-17, row 21, independently of this row) — but this
        # fixture is unreachable for web regardless: the module-level guard
        # above already skips the whole file for any --app outside
        # {linux,macos,ios,tui}, since the OTHER gap this file's tests need
        # (driving the admin-claim onboarding UI itself) is not built for
        # web/windows/android. Not this track's job — tracked at the sibling
        # `skip_unbuilt("the believable admin-claim onboarding UI drive")`
        # a few tests down.
        skip_unbuilt(
            app.driver,
            surface="the post-LoggedIn dial seam",
            detail="tui and linux both resolve the dial through the shared "
            "`fauna_launch_machine::resolved_dial_url` (tui at its "
            "`session::adopt`/`establish` call sites, linux at "
            "`launch_main_app_after_signin` + its stored-account relaunch; "
            "macos+ios now at `FaunaMacApp`/`FaunaApp`'s "
            "`completeAuthenticatedLaunch`/`enterOptimistically` + "
            "`applySessionPatch` call sites). windows/android still dial "
            "the literal typed URL; each owes the same one-call swap at its "
            "own store read ",
            tracked="onboarding.md",
        )

    yield from _real_domain_nest(
        request, app, nest_mode, tmp_path_factory, "admin-claim-real-domain-nest"
    )


@pytest.fixture
def unclaimed_social_only_real_domain_nest(request, app, nest_mode, tmp_path_factory):
    """`unclaimed_real_domain_nest` on a box whose own build declares **no
    content scanners** — a social-only box: its topology hatch carries
    `content_scanners = "absent"`, so `fauna.setup.status` reports it and the
    nest refuses a mail enable (`mail-bridge-lifecycle.md` § Default-off on first
    claim → *A box with no content scanners cannot enable mail*).

    The Docker image's entrypoint writes that hatch; here it is seeded into the
    data dir before a restart in place, since the nest reads it once at boot —
    the seam `tests/api/test_mail_unavailable_without_scanners.py` proves at the
    API layer. An image nest regenerates its hatch on every start, so this
    fixture is local-nest only."""
    from helpers import nest_mode as nest_mode_mod
    from helpers.app_surface import skip_environment

    if not nest_mode_mod.builds_local_nest(nest_mode):
        skip_environment(
            "the hatch is seeded into a locally-spawned nest's data dir; an image "
            "nest's entrypoint regenerates it on every start"
        )
    yield from _real_domain_nest(
        request, app, nest_mode, tmp_path_factory, "admin-claim-social-only-nest",
        content_scanners="absent",
    )


def _real_domain_nest(
    request, app, nest_mode, tmp_path_factory, name, *, content_scanners=None
):
    """The body of the real-domain fixtures: start the unclaimed nest, optionally
    seed its topology hatch's `content_scanners` declaration (a restart in place
    — the claim-code file survives it), then install the `"nest"` override the
    wizard and the authed session both ride, and clear it on teardown."""
    import os

    from common.nest import start_nest_in_place, stop_nest
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, name,
        handle_domain_seed=MAIL_PRIMARY_DOMAIN, unclaimed=True,
    )
    assert nest["admin"] is None, "fixture must hand the UI a genuinely unclaimed nest"
    if content_scanners is not None:
        try:
            stop_nest(nest, graceful=True)
            with open(os.path.join(nest["tmp_dir"], "operator-hatch.toml"), "w") as f:
                f.write(f'content_scanners = "{content_scanners}"\n')
            start_nest_in_place(nest)
        except BaseException:
            cleanup()
            raise
        with WsRpcAnonClient(nest["url"]) as anon:
            status = anon.call("fauna.setup.status", {})
        assert status.get("content_scanners") == content_scanners, (
            f"fixture setup failed: the seeded hatch did not reach "
            f"fauna.setup.status: {status}"
        )
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    # Every nest call — the pre-identity probe/claim AND the authed session the
    # launch glue establishes — rides this one override; nothing else can reach
    # `fauna.test`.
    app.driver.set_provider_base_urls({"nest": nest["url"]})
    try:
        yield nest
    finally:
        # An empty map is the clear: `provider_base_url("nest")` finds no entry,
        # so the machine is back to resolving handles for itself.
        try:
            app.driver.set_provider_base_urls({})
        except Exception:
            pass
        cleanup()


# ── The tests ─────────────────────────────────────────────────────────────────


def _claim_fresh_admin(app, nest) -> str:
    """Import a fresh identity and claim `nest` through the wizard with a
    LOOPBACK handle (needed for local-nest discovery with no real DNS) — the
    address type the handle-locality derived default treats as non-real, so
    mail/CalDAV/CardDAV/WebDAV all derive OFF at claim (see module
    docstring). Returns the admin's secret (hex)."""
    localpart = "admin" + secrets.token_hex(3)
    secret_hex = secrets.token_hex(32)
    typed_handle = f"{localpart}@127.0.0.1:{nest['port']}"
    _drive_admin_claim_to_logged_in(app, nest, secret_hex, typed_handle)
    return secret_hex


@pytest.mark.feature("claim-a-fresh-nest")
def test_admin_claim_with_loopback_handle_derives_mail_disabled(app, unclaimed_mail_nest):
    """Claiming with a loopback handle must NOT auto-enable deployment mail —
    the handle-locality derived default is OFF for a non-real-domain target,
    with no onboarding-time override any more (onboarding.md § 3b)."""
    if not (
        app.driver.is_linux()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="the believable admin-claim onboarding UI drive",
            detail="linux + macOS + iOS + tui have it (iOS confirmed "
            "in-process 2026-06-17); web/windows/android are the remaining "
            "cross-app follow-on",
            tracked="onboarding.md",
        )

    nest = unclaimed_mail_nest
    secret_hex = _claim_fresh_admin(app, nest)

    decided, enables = _glue_verdict(app, secret_hex, nest["url"])
    assert not decided.get("email") and enables["mail_enabled"] is False, (
        "claiming with a loopback handle must derive mail-enable OFF (no "
        "real-domain target), but the glue decided "
        f"{decided!r} and the nest reads {enables}"
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_admin_claim_with_real_domain_handle_auto_enables_mail(app, unclaimed_real_domain_nest):
    """The derived-**ON** twin of the test above, and the first local proof of it
    on any app: claiming with a **domain-shaped** handle auto-mints the admin's
    mailbox and enables deployment mail with **no user action after the claim**.

    `onboarding.md` § 3b's default rule — ON iff the handle domain is a real
    public DNS name AND the effective NAT mode is not `private`. The NAT
    conjunct holds because `_drive_admin_claim_to_logged_in` confirms the
    pre-selected seed (`public`) on `nat_mode_choice`.

    What makes this reachable locally at all is the seam this test's own track
    built: every pre-identity call already honored the runtime
    `provider_base_urls["nest"]` override (`machine.rs`'s `probe_base` /
    `effective_nest_url`), but the **authenticated** session the launch glue
    then establishes dialed the literal typed URL — `https://fauna.test`, which
    no DNS resolves — so the mint could never run. The app now dials
    `resolved_nest_dial_url()` while persisting the literal, which is why the
    assertions below can observe the mint instead of a launch error."""
    # The app gate + the `"nest"` override install both live in the fixture (it
    # runs first, so a non-tui app skips before starting a nest); the fixture
    # also clears the override on teardown, which the rest of this module
    # depends on.
    nest = unclaimed_real_domain_nest

    localpart = "admin" + secrets.token_hex(3)
    secret_hex = secrets.token_hex(32)
    _drive_admin_claim_to_logged_in(app, nest, secret_hex, f"{localpart}@{MAIL_PRIMARY_DOMAIN}")

    # No UI action between the claim and these assertions: the launch glue's
    # `provision_mail_at_first_setup` is the only thing that can flip either.
    assert _wait_deployment_mail_enabled(nest["url"], timeout=90.0), (
        "a real-domain admin claim must auto-enable deployment mail "
        "(onboarding.md § 3b derived default ON), but `email_enabled` stayed "
        f"false. app error: {app.error_text()!r}"
    )
    assert _wait_for_auto_minted_credential(app, timeout=60.0), (
        "the real-domain claim enabled deployment mail but never minted the "
        "admin's own mailbox credential. "
        f"mail-page error: {app.mail_settings.page_error_text(timeout=3.0)!r}; "
        f"app error: {app.error_text()!r}"
    )

    # onboarding.md § 3b's NAT conjunct "covers all four intents" (line 154), not
    # just mail — a real-domain, public-NAT claim must derive CalDAV/CardDAV/WebDAV
    # ON too, with no user action after the claim (same as the mail assertion
    # above). `_read_deployment_enables`'s own docstring warns `carddav_enabled`/
    # `webdav_enabled` FALL BACK to `mail_enabled` when never explicitly set, so
    # this reads all four rather than trusting the mail-only assertion above to
    # stand in for the DAV siblings.
    enables = _wait_all_deployment_enables(secret_hex, nest["url"], timeout=90.0)
    assert all(enables.values()), (
        "a real-domain admin claim must derive ALL FOUR serving intents ON "
        "(onboarding.md § 3b's NAT conjunct covers all four, not just mail), but "
        f"not every axis flipped: {enables}. app error: {app.error_text()!r}"
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_marking_the_box_private_at_claim_leaves_all_four_serving_off(
    app, unclaimed_real_domain_nest
):
    """Answering "behind my home router" on `nat_mode_choice` derives mail,
    calendar, contacts and files all OFF — on a box whose handle would
    otherwise turn them all ON.

    This is the NAT axis of § 3b's default rule ("ON iff the handle domain is a
    real public DNS name AND the box's effective NAT mode is not `private`")
    isolated, and the test above is its control: same fixture, same
    domain-shaped handle, same journey, differing in exactly one click. That
    pairing is the whole design — a private-box assertion made with a loopback
    handle would pass on handle locality alone and say nothing about the NAT
    conjunct, which is the half no end-to-end test covered.

    Why the conjunct exists, and so why this is worth a real nest: it is how the
    two-box home-relay deployment says "this box runs no mail" with no extra UI
    (`deployment-home-with-public-relay.md`). Both boxes are claimed with the
    same real-domain handle; the home box is the one on the private axis. A
    claim-time enable there mints a **fresh MSEK on the home box, diverging from
    the fleet MSEK** the link action later re-seals onto it — one MSEK per actor
    is `mail-credentials.md` § MSEK lifecycle rule 8, and a divergence is not
    self-healing. The derivation is pinned as a unit in
    `libs/fauna-onboarding-machine/tests/serving_enablement_derivation.rs`
    (`committed_private_with_real_domain_derives_all_four_off`); what a unit
    cannot say is that the radio the user actually clicks reaches that
    derivation, and that the launch glue then honours it — the glue is four
    separate fire-and-forget calls, and nothing but a real claim observes them.
    """
    nest = unclaimed_real_domain_nest

    localpart = "admin" + secrets.token_hex(3)
    secret_hex = secrets.token_hex(32)
    _drive_admin_claim_to_logged_in(
        app, nest, secret_hex, f"{localpart}@{MAIL_PRIMARY_DOMAIN}", nat_mode="private"
    )

    # No UI action between the claim and this read: whatever the launch glue
    # requested is all that can have moved these. The glue's completion anchor
    # orders the single read after every write it could make, so a late-firing
    # enable — the failure this is here to catch — cannot miss it.
    decided, ever = _glue_verdict(app, secret_hex, nest["url"])
    assert not any(ever.values()) and not any(decided.values()), (
        "a box marked private at claim must derive ALL FOUR serving intents OFF, "
        "but at least one was requested during the launch glue's window: "
        f"{ever} (the glue decided {decided!r}). The handle targets a real domain, so the ONLY thing that may "
        "hold these off is the NAT conjunct — on a home-relay box a mail enable "
        "here mints an MSEK that diverges from the fleet's. "
        f"app error: {app.error_text()!r}"
    )


# The `feature("claim-a-fresh-nest")` marker lands with the catalog edit that
# cites this test under outcome 4 (the catalog lint refuses an uncited
# witness).
def test_claiming_a_social_only_box_leaves_mail_off_and_serves_the_rest(
    app, unclaimed_social_only_real_domain_nest
):
    """A box built without mail — its own `fauna.setup.status` reports
    `content_scanners: "absent"` — claimed with a real-domain handle on the
    public axis derives the **mail** intent OFF, while calendar, contacts and
    files still derive ON and are enabled.

    `onboarding.md` § 3b → *Third axis — the box's own build*: the mail intent
    defaults ON only if `fauna_core::mail_availability::mta_can_run` holds over
    the box's report, which the wizard's setup-status probe carries; the DAV
    intents ride the HTTPS surface every box has and are untouched. The control
    is `test_admin_claim_with_real_domain_handle_auto_enables_mail`: same
    handle, same NAT answer, same journey, differing only in the box's hatch.

    Why `decided` is the witness and not `mail_enabled` alone: the nest already
    refuses `set_mail_enabled(true)` on such a box, so the flag reads false
    with or without the third axis. Without it the glue would still plan the
    mail step and run `enable_mail_with_generated_password` — minting the
    admin's mail credential before the nest's refusal fails the step. The run's
    own `decided.email == false` is what says the step was never planned, so
    there was nothing to mint and nothing to fail. (A mailbox MSEK *is* minted
    here — by the CalDAV step, which mints the shared MSEK when no earlier
    protocol did; that is § 3b's mechanism, not a mail enable.)
    """
    nest = unclaimed_social_only_real_domain_nest

    localpart = "admin" + secrets.token_hex(3)
    secret_hex = secrets.token_hex(32)
    _drive_admin_claim_to_logged_in(app, nest, secret_hex, f"{localpart}@{MAIL_PRIMARY_DOMAIN}")

    # Convention 14: the run's completion is the anchor — the glue published
    # `SERVING_ENABLEMENT_KEY` for this admin and every planned step answered.
    decided, _ = _glue_verdict(app, secret_hex, nest["url"])
    assert decided == {"email": False, "caldav": True, "carddav": True, "webdav": True}, (
        "a social-only box (content_scanners=absent) claimed with a real-domain "
        "public handle must derive mail OFF and the three DAV intents ON "
        f"(onboarding.md § 3b → Third axis), but the glue decided {decided!r}. "
        f"app error: {app.error_text()!r}"
    )
    enables = _wait_all_deployment_enables(
        secret_hex, nest["url"], timeout=90.0,
        axes=("caldav_enabled", "carddav_enabled", "webdav_enabled"),
    )
    assert enables == {
        "mail_enabled": False, "caldav_enabled": True,
        "carddav_enabled": True, "webdav_enabled": True,
    }, (
        "after the claim's serving-enablement run the box must hold mail OFF and "
        f"calendar/contacts/files ON, but reads {enables}. "
        f"app error: {app.error_text()!r}"
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_a_returning_admin_sign_in_issues_no_deployment_enable(
    app, unclaimed_real_domain_nest
):
    """§ 3b's CLAIM AXIS, at the layer the live-box defect actually lived at: a
    returning admin who merely SIGNS IN must issue no deployment-enable RPC —
    on a box whose handle domain and NAT mode would both derive ON if they were
    the only axes.

    This is `test_admin_claim_with_real_domain_handle_auto_enables_mail`'s twin,
    and deliberately runs BOTH journeys in one body against ONE nest, because
    what § 3b's gating rule asserts is a DIFFERENCE, and a difference needs both
    sides measured under the same conditions:

      leg 1 (control) — the same identity CLAIMS with a domain-shaped handle and
        confirms the pre-selected (`public`) NAT seed; the launch glue enables
        deployment mail. Everything the default rule's two axes want is true.
      leg 2 (the pin) — the deployment toggles are put back OFF, the client store
        is wiped, and the SAME identity types the SAME handle at the SAME nest.
        The only difference from leg 1 is that the wizard signs in
        (`AlreadyOnNest`) instead of claiming. Nothing may be enabled.

    So a regression cannot pass this by disabling the glue outright: leg 1 would
    go red first. And leg 2's negative is anchored on the sign-in's own
    serving-enablement completion, not a guessed window — see the anchor note
    above.

    The defect this pins, verbatim from the live box (2026-08-16): a returning
    admin signing in on a real-domain public box fired `provision_mail_at_first_
    setup` + `set_caldav_enabled(true)` + `set_carddav_enabled(true)` +
    `set_webdav_enabled(true)` against a box they had already configured — four
    Admin-class deployment writes nobody asked for. The live nest escaped only
    because that run's RPC channel happened to be down. Fixed by the
    `claim_completed` conjunct in `fauna-onboarding-machine` (so all 7 apps
    inherit it); pinned at tier_1 by `serving_enablement_derivation.rs::
    plain_sign_in_on_a_real_domain_public_box_derives_all_four_off`. This test is
    the e2e half: the tier_1 pins cover the DERIVATION, this covers the JOURNEY.

    ⚠ **Red-verified 2026-08-16 against the reverted conjunct, and the witness was
    `caldav_enabled` — NOT `mail_enabled`.** With the `claim_completed` conjunct
    deleted from all four getters, the control leg passed (its enable landed in
    4.0s) and leg 2 then failed on `['caldav_enabled']`. The mail arm did *not*
    re-fire the deployment flag, because leg 1 has already minted this admin's
    mailbox by then, so `enable_mail_with_generated_password` no longer reaches
    its final `set_mail_enabled(true)`. **That is precisely why this pin reads all
    four axes instead of mail alone: a mail-only assertion would have
    FALSE-PASSED against the real defect.** Do not narrow `_read_deployment_
    enables` to the axis whose name matches this module.
    """
    # App gate + the `"nest"` override install/clear live in the fixture.
    nest = unclaimed_real_domain_nest

    localpart = "admin" + secrets.token_hex(3)
    secret_hex = secrets.token_hex(32)
    typed_handle = f"{localpart}@{MAIL_PRIMARY_DOMAIN}"

    # ── leg 1: the control. A claim on this box DOES enable. ───────────────────
    _drive_admin_claim_to_logged_in(app, nest, secret_hex, typed_handle)
    assert _wait_deployment_mail_enabled(nest["url"], timeout=_CONTROL_ENABLE_WAIT_S), (
        "the CONTROL leg failed: a real-domain admin claim must auto-enable "
        "deployment mail (onboarding.md § 3b derived default ON), but "
        "`email_enabled` stayed false. Until this leg passes, the sign-in leg "
        "below proves nothing — a glue that enables NOTHING would also pass it. "
        f"app error: {app.error_text()!r}"
    )

    # ── back to "already configured, serving currently OFF" ────────────────────
    _disable_deployment_serving(secret_hex, nest["url"])
    before = _read_deployment_enables(secret_hex, nest["url"])
    assert not any(before.values()), (
        f"fixture setup failed: the deployment toggles did not go back off: {before}"
    )

    # Wipe the client store so the wizard runs again. The nest keeps its claim,
    # and the fixture's `provider_base_urls["nest"]` override survives a reset
    # (it lives in the long-lived OnboardingMachine — see the fixture docstring),
    # so leg 2 reaches the same nest.
    app.driver.reset()

    # The claim leg already left a COMPLETED run for this identity in the app's
    # records (unless the reset relaunched); the sign-in's own run is the one past
    # those. Read after the reset, before the drive: a relaunch zeroes the count.
    from helpers.waiting import serving_enablement_completed_runs

    actor_id_hex = _admin_keys(secret_hex)[0].hex()
    prior_runs = serving_enablement_completed_runs(app.driver, actor_id_hex)

    # ── leg 2: the pin. The same admin signs in; nothing may be enabled. ───────
    _drive_sign_in_to_logged_in(app, secret_hex, typed_handle)

    # Convention 14: the sign-in's serving-enablement run (all four intents OFF,
    # a decide-nothing plan) is the causal anchor — `completed` passing the
    # baseline is the verdict the old settle window only approximated — and ONE
    # read of the toggles after it is final.
    decided, enables = _glue_verdict(
        app, secret_hex, nest["url"], prior_runs=prior_runs
    )
    fired = sorted(axis for axis, on in enables.items() if on)
    assert not fired and not any(decided.values()), (
        f"a plain SIGN-IN enabled deployment serving: {fired} read ON after the "
        f"sign-in's serving-enablement run completed (the glue decided "
        f"{decided!r}). onboarding.md § 3b's gating rule: the two defaulting axes "
        "are evaluated ONLY on a wizard run that actually claimed the box, so a "
        "sign-in derives all four OFF regardless of handle and NAT axis — "
        "re-asserting deployment state from a client-side derivation is an "
        "unrequested Admin-class write against a box the admin already "
        f"configured. app error: {app.error_text()!r}"
    )


def test_admin_enables_mail_via_mail_settings_after_loopback_claim(app, unclaimed_mail_nest):
    """After a loopback-handle claim (mail derived OFF), the admin can still
    enable mail through the REAL mail-settings UI — the surviving client
    surface for the mint-then-flag sequence the retired onboarding checkbox
    used to fire automatically (see module docstring)."""
    if not (
        app.driver.is_linux()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="the mail-settings enable path "
            "(mail-settings-enabled-toggle + add-credential form)",
            detail="linux + macOS + iOS + tui wire it (shared FaunaKit mail "
            "settings on macOS/iOS; iOS confirmed in-process 2026-06-17). "
            "web/windows/android are the remaining cross-app follow-on",
            tracked="mail-settings.md",
        )

    nest = unclaimed_mail_nest
    _claim_fresh_admin(app, nest)

    # The admin visits Settings → Mail and enables it — the same UI gesture
    # `helpers.mail_client_ui._enable_mail_ui` and every other mail-onboarding
    # test in this suite already drive.
    app.mail_settings.navigate()
    app.mail_settings.enable_mail(display_name="Default")

    # `enable_mail`'s final step is the deployment-wide `set_mail_enabled(true)`
    # (Admin-class on the nest, so it takes effect for the admin's own enable).
    assert _wait_deployment_mail_enabled(nest["url"], timeout=60.0), (
        "enabling mail through the mail-settings UI did not enable deployment "
        "mail (fauna.bridges.set_mail_enabled did not fire or failed)."
    )

    # The admin's own `Default` mail credential was minted and is visible on
    # the mail-settings page.
    assert _wait_for_auto_minted_credential(app, timeout=60.0), (
        "enabling mail through the mail-settings UI did not mint the admin "
        "mailbox credential. "
        f"mail-page error: {app.mail_settings.page_error_text(timeout=3.0)!r}; "
        f"app error: {app.error_text()!r}"
    )
