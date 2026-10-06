"""tier_3 e2e: the POST-AUTH "nest identity changed" surface.

``docs/goal/architecture/security.md`` § Transport trust → § Post-auth surfacing
(ratified 2026-07-23); ``tests/e2e-unified/ui.yaml`` page ``launch_identity_changed``.

The **post-auth twin** of ``test_nest_identity_pin.py``. That module drives the
*launch-time* pinned challenge (seed a bad pin, relaunch, the launch flow routes to
``LaunchPhase::IdentityChanged``). This one drives the *mid-session* channel: a
already-authenticated session's background silent challenge
(``client.rs::silent_sign_in``) meets a pin the nest can no longer prove and
escalates to ``DataMessage::NestIdentityChanged`` → the SAME blocking
``launch_identity_changed`` surface, with no retry CTA and no soft banner — the
session is de-facto dead (its connections can no longer graduate), so the honest
verdict, not a toast.

**Two harness seams this exercises:**

1. **Re-seed the pin on a LIVE session.** ``set_nest_identity_pin_for_test`` is
   machine-free (it writes the process-global pin store), but linux used to route
   *every* ``call_machine_method`` through a live ``OnboardingMachine`` — of which
   there is none post-auth. It now goes through the shared free dispatcher
   (``fauna_onboarding_machine::call_machine_free_method``) *before* requiring a
   machine, so the seed reaches the store on the authenticated session.
2. **Trigger the production refresh.** The ``silent_sign_in`` bridge command runs
   the real ``FaunaClient::silent_sign_in`` — a trigger for the production path
   (``silent_sign_in`` → ``classify_silent_challenge`` →
   ``DataMessage::NestIdentityChanged`` → the handler → the launch surface), NOT a
   shortcut that fakes the verdict.

**Which apps run.** linux was the first wired leg (Track 10); tui
is the second (2026-07-30); macos/ios are the third and fourth (2026-08-01). Each
wires the same three pieces: the shared classifier verdict, an agent
``silent_sign_in`` command driving the production refresh, and a post-auth
handler that re-enters the real launch flow. linux/tui reach the classifier
directly (``fauna_launch_machine::classify_silent_challenge`` — lifted out of
linux when tui joined, native Rust); apple goes over UniFFI instead — the same
verdict distinction surfaces as a thrown ``FfiError.NestIdentityChanged`` from
``APIClient.silentSignIn(secret:)`` (``libs/fauna-ffi/src/auth.rs``), caught and
routed by the platform's `#if DEBUG` test command + production handler. The
remaining three (android/windows/web) join by adding those and a name to
``_POST_AUTH_APPS``.

**Single live process, no relaunch.** Unlike the launch-time module it never
relaunches. ``self_signed_nest`` serves real (self-signed) HTTPS — native pins are
scheme-gated to ``https://``, so the disagreeing pin fails possession-verify as
``IdentityError::PinChanged`` (the Changed arm).

Reaching an authenticated *linux* session injects the identity into libsecret at
boot, so the linux arm runs a private ``gnome-keyring-daemon`` of its own
(``LibsecretCredStore``; same gate as the launch-time linux arm) and skips only
where that daemon is not installed — no desktop session is needed. tui's and
apple's credential stores are file-backed (``cred_store.requires_secret_service``
is linux-only), so they need nothing at all.
"""

import json

import pytest

from common.cred_store import requires_secret_service
from common.keyring import secret_service_available
from common.launch_harness import make_launch_harness, reached_authenticated_app
from conftest import _trust_seeder, get_available_apps
from helpers.app_log_section import app_text
from helpers.app_surface import skip_environment, skip_unbuilt
from helpers.budgets import IDENTITY_REFRESH_S
from helpers.waiting import session_generation

pytestmark = [
    pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui,
    pytest.mark.macos, pytest.mark.ios, pytest.mark.web, pytest.mark.windows,
]

#: Apps with the post-auth verdict wired end-to-end (classifier → agent
#: ``silent_sign_in`` → handler → the blocking launch surface). An app joins by
#: building those three, not by widening a marker.
#: windows joined 2026-08-31: the
#: free-dispatch fallback in TestAgent.cs's call_machine_method (a throwaway
#: OnboardingMachine for the two pin-store names when OnboardingViewModel.Current
#: is null post-auth) plus a new silent_sign_in command
#: (App.RunSilentSignInForTestAsync, driving the SAME LaunchMachine.RefreshToken()
#: RunTtlRefreshLoopAsync's own tick calls).
_POST_AUTH_APPS = ("linux", "tui", "macos", "ios", "web", "windows")

# Element IDs (tests/e2e-unified/ui.yaml § launch_identity_changed).
IDENTITY_WARNING = "nest-identity-changed-warning"
IDENTITY_TRUST_BUTTON = "nest-identity-changed-trust-button"
LAUNCH_RETRY = "launch-retry-button"

#: A nest_actor_id no nest can ever prove possession of.
BOGUS_PIN = "ab" * 32


def _post_auth_apps():
    available = get_available_apps()
    return [c for c in _POST_AUTH_APPS if c in available]


@pytest.fixture(params=_post_auth_apps())
def pin_app(request):
    """The app under test; its id lands in the test name (``[linux]``/``[tui]``),
    which is what conftest's ``--app`` filter reads."""
    return request.param


@pytest.fixture(autouse=True)
def _require_credential_persistence(pin_app):
    """linux's adapter runs a private Secret Service daemon of its own and skips
    only where the box cannot supply one; tui's file backend needs nothing and
    never skips."""
    if requires_secret_service(pin_app) and not secret_service_available():
        skip_environment(
            "linux's real-keyring launches run a private gnome-keyring-daemon, which this box cannot supply"
        )


@pytest.fixture
def pin_env(request, pin_app, tmp_path):
    """A per-app launch harness over ``self_signed_nest`` (real TLS → the Changed
    arm) plus the identity it authenticates with.

    **web is the one WITHDRAWN arm** (same split as the launch-time module): it
    rides its plain-HTTP ``spa_url`` origin, which serves no ``cert_binding`` at
    all, so a seeded pin cannot be *confirmed* rather than being contradicted —
    ``WebIdentityError::Withdrawn`` instead of ``Changed``. Both are the same
    verdict class and reach the same surface; only the re-TOFU tail differs (a
    plain-HTTP nest has no binding to re-pin, so the pin is simply forgotten)."""
    if pin_app == "web":
        spa_url = request.getfixturevalue("spa_url")
        user = request.getfixturevalue("test_user")
        harness = make_launch_harness("web", spa_url=spa_url)
        env = {
            "harness": harness,
            "node_url": spa_url,
            "nest": None,  # a browser reads no environment
            "secret_hex": bytes(user["signing_key"]).hex(),
            "arm": "withdrawn",
        }
        try:
            yield env
        finally:
            try:
                harness.teardown()
            except Exception:
                pass
        return
    nest = request.getfixturevalue("self_signed_nest")
    if pin_app == "ios":
        # No bare `ios_app_path` fixture exists — iOS's direct-launch fixture
        # (`ios_setup`) returns `{"udid", "app_path"}` together, because
        # `drivers/ios.py`'s `launch()` requires both. Same branch as
        # `test_nest_identity_pin.py`'s `pin_env`.
        ios_setup = request.getfixturevalue("ios_setup")
        harness = make_launch_harness(
            "ios", tmp_path=tmp_path, app_path=ios_setup["app_path"],
            udid=ios_setup["udid"], seed_trust=_trust_seeder(request),
        )
    else:
        app_path = request.getfixturevalue(f"{pin_app}_app_path")
        harness = make_launch_harness(
            pin_app, tmp_path=tmp_path, app_path=app_path, seed_trust=_trust_seeder(request)
        )
    env = {
        "harness": harness,
        "node_url": nest["url"],
        "nest": nest,
        "secret_hex": bytes(nest["user"]["signing_key"]).hex(),
        "arm": "changed",
    }
    try:
        yield env
    finally:
        try:
            harness.teardown()
        except Exception:
            pass


def _app_diagnostics(driver):
    """Whatever this app can say about why it did not paint the surface.

    Rule 6 — a failure must diagnose itself. The app's own words come through
    the ONE shared surfacing path (``helpers/app_log_section.app_text``) rather
    than a hand-picked attribute name, because the families spell it
    differently: linux/tui/macos/windows keep captured stderr
    (``app_stderr_text``), **ios keeps an on-disk ``app_log_text``**. This
    reader named the stderr spelling alone until 2026-09-21, so every ios red in
    this module reported the bare sentence "no diagnostics available on this
    app" while the app's own log sat unread in its container — which is most of
    why the escalation red below took a dedicated session to read. Web has neither attribute (it is a browser);
    its equivalent is the bridge's console + ``pageerror`` ring, which survives
    the navigation this escalation performs — and the escalation logs its own
    decision there, so the ring says how far the chain got.

    **The identity lines are the headline, and they are meant to be COUNTED.**
    Every app logs one line per post-auth identity verdict (the wording is
    per-app, "identity" is not), so their number answers the question every red
    in this module raises first: did the escalation under test paint this
    surface, or had an *earlier* one already painted it — in which case a
    ``wait_for`` that returned instantly was reading a stale surface, not a
    caused one.
    """
    parts = []
    text = app_text(driver)
    if text:
        lines = text.splitlines()
        ident = [ln for ln in lines if "identity" in ln.lower()]
        parts.append(f"app log ({len(ident)} identity lines): {ident[-25:]!r}")
        loud = [
            ln for ln in lines
            if any(m in ln for m in ("ERROR", "WARN", "panic", "PANIC"))
        ]
        parts.append(f"app log (loud lines): {loud[-40:]!r}")
    console = getattr(driver, "console_log", None)
    if callable(console):
        try:
            lines = console() or []
            hits = [ln for ln in lines if "identity" in ln.lower()]
            parts.append(f"console (identity lines): {hits[-20:]!r}")
            parts.append(f"console (tail): {lines[-25:]!r}")
        except Exception as exc:
            parts.append(f"console unavailable: {exc!r}")
    return " | ".join(parts) if parts else "no diagnostics available on this app"


def _launch_authenticated(env):
    """Bring the app up **already authenticated** — the precondition this whole
    module tests *from*.

    Native binaries authenticate straight out of ``launch()`` (the harness hands
    the process a data dir with the identity in it). Web's ``launch()``
    deliberately stops one step short: it seeds ``localStorage`` and leaves the SPA
    on the unauthenticated reset surface, because the launch-time module needs that
    gap to run its FIRST pinned challenge on the reload. Here we want the opposite —
    the session must already be live before the pin is poisoned — so web takes
    ``launch_and_route()``, whose reload IS that first (clean, still-unpinned)
    launch."""
    if env["arm"] == "withdrawn":
        return env["harness"].launch_and_route(
            secret_hex=env["secret_hex"], node_url=env["node_url"], trust=env["nest"]
        )
    return env["harness"].launch(
        secret_hex=env["secret_hex"], node_url=env["node_url"], trust=env["nest"]
    )


def _seed_pin(driver, nest_url, actor_id_hex):
    """Seed a TOFU pin through the shared machine-free bridge — this is what must
    reach the store on the LIVE authenticated session (seam 1)."""
    driver.call_machine_method(
        "set_nest_identity_pin_for_test",
        json.dumps({"nest_url": nest_url, "actor_id": actor_id_hex}),
    )


def _read_pin(driver, nest_url):
    """The pin the installed ``DiskPinStore`` now holds for ``nest_url`` (hex, or
    None). Accepts either wrap convention (the native agent hands back the
    already-unwrapped string; be tolerant like the launch-time module)."""
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


@pytest.mark.feature("connect-and-sign-in")
def test_post_auth_identity_change_warns_then_recovers(pin_env):
    """Authenticated session → re-seed a pin the nest can't prove → trigger the
    background refresh → the identity-changed surface blocks the session; the trust
    button re-TOFUs and lands the user back in the app.

    The trust button is the dead-button trap the whole design avoids: it drives
    ``trust_nest_identity()`` on the machine that PRODUCED the verdict, so the
    handler re-enters the real launch flow (never a synthesized in-place phase).
    Clicking it must actually get the user back in.

    **Read the anchor block below before touching the order of these steps.**
    Both observables this test compares against are taken while the pin still
    holds the nest's genuine identity, because that is the only moment at which
    "no escalation has happened yet" is provable rather than assumed."""
    env = pin_env
    driver = _launch_authenticated(env)

    # Reach the authenticated session; the launch-time silent challenge TOFU-pins
    # the nest's REAL identity. Wait for that baseline to settle BEFORE overwriting
    # it, or the async TOFU races the seed and re-pins the real id.
    reached_authenticated_app(driver, timeout=90)

    # ── The causal anchor, taken while the pin is still the nest's GENUINE one
    #
    # Both observables the assertions below compare against are read HERE, before
    # the pin is poisoned, and that ordering IS the soundness argument: an
    # identity-changed verdict can only come from a challenge that read a pin the
    # nest cannot prove, so with the pin still genuine no escalation can yet have
    # happened — and everything observed after the seed is therefore downstream of
    # the seed.
    #
    # ⚠ **Reading them after the seed is unsound, and that is what made this test
    # red on ios**. Every app ALSO runs a boot-time
    # post-auth re-check of its own — one-shot, decoupled, fire-and-forget (apple
    # dispatches it from the post-auth glue, `FaunaApp.swift` /
    # `FaunaMacApp.swift`'s `Task { await performPostAuthSilentSignIn() }`; linux
    # from `FaunaClient::silent_sign_in()` in `main.rs`; each of the other four
    # its own) — and on apple that glue runs well AFTER the `session.authenticated`
    # flip `reached_authenticated_app` waits on. So the seed landed inside the
    # one-shot's window, the ONE-SHOT produced the verdict, and the trigger below
    # then found a session already torn down (`PostAuthAccountFlows.swift`'s
    # `guard let ... session.secretHex`) and returned without a second teardown.
    # With `generation_before` read after the seed it had captured the one-shot's
    # own bump, so the final assertion compared a number with itself (`assert 1 >
    # 1`) — while `wait_for` sailed through against a surface painted before its
    # own trigger, the stale-surface pass convention 14 exists to forbid.
    #
    # Anchored here, neither reading can be borrowed from an earlier escalation.
    # WHICH channel re-checked is deliberately not asserted: `security.md`
    # § Post-auth surfacing ratifies the surface "from *any* of the three
    # channels", so the claim under test is that a pin the nest can no longer
    # prove blocks the session and counts itself as a teardown — not that one
    # particular prompt was the one that noticed.
    assert driver.is_absent(IDENTITY_WARNING), (
        "the identity-changed surface must NOT be up while the pin still holds "
        "the nest's genuine identity — it is, so an escalation already ran and "
        "every assertion below would be reading a surface this test did not "
        f"cause. {_app_diagnostics(driver)}"
    )

    # Convention 14: this escalation is a session
    # teardown exactly like sign-out/switch/factory-reset and must count itself
    # the same way, or a negative assert elsewhere ("did this gesture relaunch
    # me?") would silently misreport across this one arm. Apps that have not built
    # the counter yet declare the gap rather than skip the rest of this
    # (still-valid) positive test.
    generation_before = session_generation(driver)
    if generation_before is None:
        skip_unbuilt(
            driver,
            surface="the session_generation teardown counter",
            detail="mid-session identity-changed escalation must bump it at "
            "initiation, same as sign-out/switch/factory-reset",
            tracked="",
        )

    # Seam 1: re-seed the pin on the LIVE session (no onboarding machine here).
    _seed_pin(driver, env["node_url"], BOGUS_PIN)
    assert _read_pin(driver, env["node_url"]) == BOGUS_PIN, (
        "the machine-free bridge must seed the pin store on an authenticated "
        "session; if this is the real nest id, the seed was silently dropped "
        "(the pre-seam post-auth behavior this track fixes)"
    )

    # Seam 2: trigger the production background refresh. call_command blocks until
    # the agent acks the command; the verdict then routes to the surface. On an app
    # whose boot-time one-shot is still in flight this is the SECOND live challenge
    # against the poisoned pin and either may be the one that escalates — which is
    # exactly what the anchor above makes harmless.
    driver.call_command("silent_sign_in")

    try:
        driver.wait_for(IDENTITY_WARNING, timeout=IDENTITY_REFRESH_S)
    except TimeoutError as e:
        raise AssertionError(f"{e}. {_app_diagnostics(driver)}") from e
    assert driver.is_visible(IDENTITY_WARNING), (
        "a mid-session identity the nest can no longer prove must block the session "
        "and paint the identity-changed surface"
    )
    # The warning being visible means the escalation handler already ran to
    # completion (it re-enters the launch flow before painting this surface),
    # so the counter — bumped synchronously at initiation, before any of that
    # — must already reflect it. And because the baseline was taken while the
    # pin was still genuine, a counter still sitting at it cannot be explained
    # by an earlier escalation: it means the teardown that painted this surface
    # skipped the bump, which is the silent undercount convention 14's
    # "no arm can relaunch without counting itself" exists to forbid (the defect
    # class tui had on the same two verdicts — and iOS had on its FIFTH teardown
    # arm, `leaveAuthenticatedSession`, until 2026-09-21: the connection half of `security.md` § Post-auth surfacing reaches
    # only that arm, so every escalation arriving over a bearer re-mint or an
    # SPKI re-handshake tore the session down and counted nothing).
    assert session_generation(driver) > generation_before, (
        "the identity-changed escalation must count itself as a session "
        "teardown (session_generation), same as every other teardown arm. The "
        "surface is up, so a verdict was produced and acted on; a counter still "
        "at the value it held while the pin was genuine means the teardown that "
        f"painted it skipped the bump. {_app_diagnostics(driver)}"
    )
    assert driver.is_visible(IDENTITY_TRUST_BUTTON), (
        "the surface must offer an explicit 'trust this nest' re-pin action"
    )
    assert driver.is_absent(LAUNCH_RETRY), (
        "a possible-MITM signal must NOT offer a Retry CTA; "
        "launch_identity_changed carries no launch-retry-button"
    )

    # Trust → forget the pin → re-run the challenge → back in the app. A dead
    # button hangs here.
    driver.click(IDENTITY_TRUST_BUTTON)
    try:
        reached_authenticated_app(driver, timeout=60)
    except (TimeoutError, AssertionError) as e:
        # Rule 6 — the failure must diagnose itself. Bare, this timeout says only
        # "still not authenticated", which reads as a hang and is equally
        # consistent with (a) a dead button, (b) a button acting on the WRONG
        # launch machine, and (c) a re-auth that succeeded and was torn straight
        # back down. Those look identical from outside and cost three runs across two sessions to tell apart; the app's own
        # `[trust-nest] acting on launch phase …` line names the machine the
        # click actually got, and a repeated `[post-auth] … re-entering launch`
        # is case (c)'s signature.
        raise AssertionError(
            f"the trust button did not land the user back in the app: {e}. "
            f"{_app_diagnostics(driver)}"
        ) from e
    assert driver.is_absent(IDENTITY_WARNING), (
        "after re-trusting, the warning should be gone and the user in the app"
    )

    # The stale pin was forgotten. The Changed arm re-TOFU'd the nest's genuine
    # identity over the live TLS binding; the Withdrawn arm's plain-HTTP nest
    # serves no binding, so there is nothing to re-pin and the origin's pin is
    # simply cleared. Both must have dropped the bogus one.
    repinned = _read_pin(driver, env["node_url"])
    assert repinned != BOGUS_PIN, (
        f"the stale pin must be forgotten by the re-trust action, got {repinned!r}"
    )
    if env["arm"] == "withdrawn":
        assert repinned is None, (
            "a plain-HTTP nest serves no binding, so the re-TOFU has nothing to "
            f"re-pin — the origin's pin should be forgotten, got {repinned!r}"
        )
    else:
        assert repinned is not None, (
            f"re-TOFU against a real TLS binding must re-pin the nest's genuine "
            f"identity, got {repinned!r}"
        )


def test_post_auth_valid_refresh_does_not_escalate(pin_env):
    """A background refresh whose verdict is NOT identity-changed must be
    swallowed: the session stays authenticated and never paints the surface.

    The transient / secret-invalid / needs-update arms are unit-pinned in
    ``silent_sign_in_classification_tests`` (only the identity verdict escalates);
    this is the end-to-end that a *valid* refresh does not spuriously tear the
    session down."""
    env = pin_env
    driver = _launch_authenticated(env)
    reached_authenticated_app(driver, timeout=90)

    # The pin holds the nest's REAL identity (first-contact TOFU). Trigger the
    # production refresh again — the verdict is Refreshed, which silent_sign_in
    # swallows. The identity-changed surface is only ever painted by a
    # NestIdentityChanged verdict, which a valid refresh never produces, so this
    # negative has no race: nothing a valid refresh does can make the warning
    # visible.
    driver.call_command("silent_sign_in")
    assert driver.is_absent(IDENTITY_WARNING), (
        "a refresh with a still-valid identity must NOT paint the identity-changed "
        "surface — only the identity verdict escalates (security.md § Post-auth)"
    )
    # And the session is intact (returns immediately when still authenticated).
    reached_authenticated_app(driver, timeout=15)

    # Causal barrier (convention 14): prove the pipeline is still live after the
    # valid refresh — a genuine identity change DOES fire. If the valid refresh had
    # torn the session down, this seed+trigger could not reach the live surface.
    _seed_pin(driver, env["node_url"], BOGUS_PIN)
    driver.call_command("silent_sign_in")
    driver.wait_for(IDENTITY_WARNING, timeout=IDENTITY_REFRESH_S)
    assert driver.is_visible(IDENTITY_WARNING), (
        "after a benign refresh, the identity pipeline must still escalate a real "
        "change — the benign one neither tore down nor wedged it"
    )
