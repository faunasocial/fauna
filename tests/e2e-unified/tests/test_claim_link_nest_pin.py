"""tier_3 e2e: a claim made with the printed link holds the nest to the identity
the link names — the platform-neutral (native) arm.

`docs/goal/behavior/onboarding.md` § 3a. Claim code: "A recognized URI's `nest=`,
when present and 64-hex, is held as the first-contact identity root for the claim
host (`hold_first_contact_identity`), so the very connection `claim_admin` opens
graduates against the identity the console vouched for — no TOFU window."
Trust-model authority: `docs/goal/architecture/security.md` § Transport trust.

**Why this file exists beside `test_web_claim_pin_wasm_witness.py`.** That module
is `pytest.mark.web` and witnesses the *wasm* store's re-seed guard
(`LocalStoragePinStore`, same-root vs differing-root). This one witnesses the
property the outcome actually states — that the link's `nest=` decides whether
the claim happens at all — on the native clients, whose pin rides the
connect-time channel binding rather than localStorage. It is an ADDITIONAL
witness, never a replacement: the two observe different stores through different
code paths.

⚠ **The TLS is not optional, and this is the one thing that does not lift from
the web module.** Native graduation is gated on the `https://` scheme
(`fauna-launch-machine/src/auth.rs` + `fauna-anon-client/src/bearer.rs`), and
`fauna_anon_client::graduate_first_contact` returns `SkippedPlaintext` outright
for `http://`/`ws://` (`libs/fauna-anon-client/src/client.rs`). Against the
ordinary plain-HTTP harness nest a held root would therefore never be checked,
and the refusal case below would pass for the wrong reason — it would claim
happily and prove nothing. So this rides `serve_tls=True`, bound to 127.0.0.1,
which native clients accept on the loopback branch of the trust posture (the
same fixture shape `test_mail_enable_at_admin_claim.py` uses for its real
claims). The web module's own docstring notes a fabricated root failing over
plain HTTP; that is the wasm path's separate channel-binding check and does not
transfer here.

The two cases are the outcome's own two clauses, and the refusal one is the
load-bearing half: a box that claimed *anyway* under a link naming someone else
is precisely the "claimed unprotected" state § 3a exists to prevent.
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

import pytest

_e2e_dir = str(Path(__file__).resolve().parent.parent)
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from common.nest import CLAIM_CODE
from helpers.authenticated_shell import wait_for_authenticated_shell
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.claim_banner import claim_uri, read_real_nest_actor_id_hex
from helpers.app_surface import skip_unbuilt
from helpers.crash_recovery import setup_status
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.tier1]

#: A well-formed 64-hex root that is NOT this box's. Never dialed — it only ever
#: rides inside the claim URI — so it need not belong to any real nest; what it
#: must be is *parseable*, so the refusal under test is the identity check and
#: not the parser's own `Invalid` arm (which `onboarding.md` § 3a assigns to a
#: malformed `nest=`, and which outcome 8 already witnesses elsewhere).
ROOT_OF_ANOTHER_BOX = "ab" * 32


def _require_admin_claim_drive(app) -> None:
    """Declare the apps that cannot drive this journey, before any nest starts.

    Both tests walk the *believable* admin-claim onboarding drive — a typed
    loopback handle, the real handle probe, then `claim_code` — because that is
    what reaches a TLS nest the production way. That drive is built on linux,
    macOS, iOS and tui only; this is the same gap, under the same surface
    string, that `test_mail_enable_at_admin_claim.py` declares, so a sweep
    reports it once and honestly rather than as reds (convention 7).

    web is the one worth spelling out: it could not use this module even with
    the drive built, because its e2e origin is never TLS, so there is no
    channel binding for a held root to be checked against. Its outcome-9
    column is witnessed instead by `test_web_claim_pin_wasm_witness.py`,
    through the wasm store's own guard.
    """
    if not (
        app.driver.is_linux()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="the believable admin-claim onboarding UI drive",
            detail="linux + macOS + iOS + tui have it; windows and android "
            "are the remaining cross-app follow-on. web is witnessed for "
            "this outcome by test_web_claim_pin_wasm_witness.py instead — its "
            "e2e origin is never TLS, so a native channel-binding pin has "
            "nothing to check against there",
            tracked="onboarding.md",
        )


@pytest.fixture
def unclaimed_tls_nest(app, request, nest_mode, tmp_path_factory):
    """A fresh, never-claimed nest serving REAL self-signed HTTPS on loopback.

    `unclaimed=True` leaves it with no admin and the harness's `CLAIM_CODE` on
    disk, so the UI drives a genuine `fauna.auth.claim_admin`; `serve_tls=True`
    is what makes the graduation the test is about actually run (module
    docstring). Both options are honoured by every nest-mode provider.

    Gated here rather than in the test bodies: fixtures run first, so a gate in
    the body would start (and tear down) a TLS nest only to skip past it.
    """
    _require_admin_claim_drive(app)

    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "claim-link-pin-nest",
        unclaimed=True, serve_tls=True,
    )
    assert nest["admin"] is None, (
        "fixture must hand the UI a genuinely unclaimed nest — a pre-claimed one "
        "would make both cases below vacuous"
    )
    assert nest["url"].startswith("https://"), (
        f"serve_tls nest must report an https url, got {nest['url']!r} — over "
        "plain HTTP the held root is never checked and the refusal case would "
        "pass for the wrong reason"
    )
    try:
        yield nest
    finally:
        cleanup()


def _drive_claim_with(app, nest, secret_hex: str, claim_input: str) -> None:
    """Walk the real wizard to `claim_code` and submit `claim_input` verbatim.

    Stops at the submit: what happens next is what each test asserts. The
    handle is the loopback `…@127.0.0.1:{port}` form, which under uniform-https
    resolves straight at this nest — so no `set_provider_base_urls` override is
    needed (the idiom `test_mail_enable_at_admin_claim.py` established).
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    ob.fill_handle(f"admin@127.0.0.1:{nest['port']}")
    ob.run_handle_check(timeout=45)
    ob.submit_handle()

    app.driver.wait_for("claim-code-input", timeout=RPC_ROUNDTRIP_S)
    app.driver.clear_and_type("claim-code-input", claim_input)
    app.driver.click("claim-code-submit-button")


def _status(app) -> str:
    """The claim page's status text — or a marker once the page is gone.

    `claim-code-status` exists only while the wizard sits on `claim_code`; a
    claim that SUCCEEDS moves on to `nat_mode_choice` and the element goes with
    it. This must not raise then, because the one moment a failure message most
    needs it is the refusal case's own failure — a wrong link that claimed the
    box anyway. The mutation round that proved this module's refusal case (drop
    the first-contact hold → the case goes red) found exactly that: the assertion
    fired correctly, and then its message crashed on the missing element and
    buried the diagnosis under `LookupError: not found` (convention 6).

    Any exception, not just `LookupError`: drivers differ in what a missing
    element raises, and this module is platform-neutral — the same tolerance
    `test_pending_invite_journey.py::_status_text` takes for the same reason.

    Also safe inside `_wait_for_claim_to_settle`: the marker is not a
    non-terminal value, but with the page gone `claim-code-submit-button` is
    absent too and `is_enabled` answers False, so it can never be read as a
    refusal — the loop keeps polling until `nat_mode_choice` shows.
    """
    try:
        return app.driver.get_text("claim-code-status").strip()
    except Exception as exc:  # a read failure is itself a reportable fact
        return (
            f"<claim-code-status unreadable ({type(exc).__name__}) — most "
            "likely the wizard has moved past the claim page>"
        )


def _wait_for_claim_to_settle(app) -> str:
    """Block until the submitted claim reaches a TERMINAL state, and say which.

    Returns ``"claimed"`` when the wizard has left `claim_code` for
    `nat_mode_choice` (§ 3b-bis), or ``"refused"`` when the page's status has
    moved off both non-terminal values — the idle prompt and the in-flight
    "Claiming…" — onto a refusal (`ClaimCodeState::Invalid` / `Error`, both of
    which re-enable Submit).

    Convention 14: the loop ends on an OBSERVED terminal state. It never
    concludes "refused" from time passing — a slow box would otherwise make the
    refusal case pass vacuously, which is exactly the latency-dependent absence
    assertion the convention forbids. The ceiling only guards a hang, and
    hitting it raises (convention 6), naming what the page was showing.
    """
    non_terminal = {
        "",
        S.onboarding.claim_code.idle,
        S.onboarding.claim_code.submitting,
    }
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        if app.onboarding.nat_mode_showing():
            return "claimed"
        if _status(app) not in non_terminal and app.driver.is_enabled(
            "claim-code-submit-button"
        ):
            return "refused"
        time.sleep(0.5)
    raise AssertionError(
        f"the claim never settled within {RPC_ROUNDTRIP_S}s — neither the "
        f"post-claim page nor a terminal refusal appeared. status: "
        f"{_status(app)!r}; submit enabled: "
        f"{app.driver.is_enabled('claim-code-submit-button')}"
    )


def _read_pin(driver, nest_url: str):
    """The pin the installed store now holds for `nest_url` (hex, or None).

    Tolerant of both agent conventions for a value-returning machine method,
    exactly as `test_nest_identity_pin.py::_read_pin` is — web hands back the
    JSON verbatim, the native agents hand back the already-unwrapped string.
    """
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


@pytest.mark.feature("claim-a-fresh-nest")
def test_a_claim_link_naming_another_nest_is_refused(app, unclaimed_tls_nest):
    """A link whose `nest=` names a DIFFERENT box does not claim this one.

    This is the clause the outcome is really about. The wizard holds the link's
    root as the first-contact identity before it claims, so the claim
    connection graduates against a root this box cannot prove and the claim
    fails — leaving the nest unclaimed and re-claimable, rather than claimed
    under a pin that names someone else.

    The assertion is deliberately "did NOT reach the post-claim wizard", not a
    message match: § 3a fixes the behaviour, while the surfaced cause string is
    the machine's business and has changed before.
    """
    secret_hex = bytes(range(8, 40)).hex()

    _drive_claim_with(
        app, unclaimed_tls_nest, secret_hex,
        claim_uri(CLAIM_CODE, ROOT_OF_ANOTHER_BOX),
    )

    outcome = _wait_for_claim_to_settle(app)

    assert outcome == "refused", (
        "a claim link naming a DIFFERENT nest claimed this box anyway — the "
        "held first-contact root was never checked, so the box is now claimed "
        "under a pin that names someone else (onboarding.md § 3a: 'no TOFU "
        f"window'). status: {_status(app)!r}"
    )
    # ⚠ The wrong-cause guard. A TLS dial that failed for some unrelated reason
    # also leaves the admin on this page, and would make this test green
    # WITHOUT the identity check ever running. The positive twin below rules
    # out a generally broken fixture, but not a flake on this one run — so name
    # the one refusal that would be a false pass and exclude it.
    assert _status(app) != S.onboarding.claim_code.error.transient, (
        "the claim was refused only because the nest could not be REACHED, so "
        "this run says nothing about whether the link's identity was checked. "
        f"status: {_status(app)!r}"
    )
    assert app.driver.is_visible("claim-code-input"), (
        "the refusal must leave the admin ON the claim page, able to paste the "
        "real link — a dead end here would be an unrecoverable client state. "
        f"{app.driver.tree()}"
    )
    # "Refused rather than claimed UNPROTECTED" is a fact about the box, not
    # the screen: the graduation fails on the claim connection before
    # `fauna.auth.claim_admin` is ever sent, so the nest must still be
    # claimable by the admin holding the real link.
    assert setup_status(unclaimed_tls_nest["url"]).get("claimed") is False, (
        "the app showed a refusal but the nest is claimed anyway — the "
        "identity check ran too late to protect it"
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_a_claim_link_naming_this_nest_claims_it_and_pins_that_identity(
    app, unclaimed_tls_nest
):
    """The positive clause: the console's own link claims the box, and the
    identity it named is what ends up pinned.

    The root comes from the nest's own claim banner — the same out-of-band
    channel a real admin reads it from — so this asserts the whole sentence
    end to end rather than that some pin exists.
    """
    real_root = read_real_nest_actor_id_hex(unclaimed_tls_nest)
    assert real_root != ROOT_OF_ANOTHER_BOX, (
        "fixture sanity: the refusal case's stand-in root must not collide with "
        "the box's real one"
    )
    secret_hex = bytes(range(9, 41)).hex()

    _drive_claim_with(
        app, unclaimed_tls_nest, secret_hex, claim_uri(CLAIM_CODE, real_root),
    )

    assert _wait_for_claim_to_settle(app) == "claimed", (
        "the console's OWN link was refused — the box cannot be claimed with "
        f"the identity it printed. status: {_status(app)!r}"
    )
    # The control for the refusal case's server-side assertion: the same read,
    # on the same fixture shape, must flip. Without it a `setup_status` that
    # always answered `claimed: False` would make the refusal case vacuous.
    assert setup_status(unclaimed_tls_nest["url"]).get("claimed") is True, (
        "the wizard advanced past the claim but the nest reports itself "
        "unclaimed"
    )
    app.onboarding.finish_nat_mode()
    wait_for_authenticated_shell(app)

    pinned = _read_pin(app.driver, unclaimed_tls_nest["url"])
    assert pinned is not None, (
        "the claim succeeded but nothing was pinned for "
        f"{unclaimed_tls_nest['url']!r} — seed_identity_pin_at_claim never ran, "
        "so the next launch has no root to check and the link's whole promise "
        "is lost"
    )
    assert str(pinned).split("@", 1)[0].lower() == real_root.lower(), (
        "the claim pinned an identity the link did not name: pinned "
        f"{pinned!r}, link named {real_root!r}"
    )
    assert not app.has_error(), f"unexpected error after the claim: {app.error_text()!r}"
