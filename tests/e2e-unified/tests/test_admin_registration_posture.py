"""The admin sets the nest's registration posture from their client.

`fauna.admin.set_registration_mode` has been live, Admin-gated and conformance-
tested nest-side since 2026-07-12 — but no client could call it, so a deployed
nest's posture was reachable only through a pre-claim `[nest] registration_mode`
seed or a raw WS-RPC call. That is the gap the *only configuration surface is the
apps* invariant forbids, and what these tests pin closed: the mutation is driven
**through the admin UI**, never over the wire (e2e convention 8). A raw-RPC version
of this test would green while the section is broken and no admin can reach it.

Goal docs: `admin.md` § 2 Users → *Section 2 — Registration* (the section + the one
save → one kind contract); `public-mode.md` § Registration Modes (the posture
semantics — three modes, the orthogonal ceiling, and "the mode gates registration,
never authentication").

`apps/fauna-web` was the first client to render the section; `android` (the
Kotlin/Compose `AdminUsersScreen.kt` § Section 2, 2026-07-16) is the second;
`linux` (`views/admin.rs` § Section 2, 2026-07-16) is the third; `tui`
(`admin/users.rs::registration_section`, 2026-07-19) is the fourth; macOS +
iOS (shared FaunaKit `AdminUsersHubView.registrationSection`) are the fifth
and sixth; `windows` (`AdminUsersViewModel.SaveRegistrationAsync` +
`AdminUsersPage`'s Registration section, 2026-08-26) is the seventh and
last — all 7 apps render the section. Android's e2e run is host-emulator-gated
(not runnable on the primary dev VM), so its leg is compile+Robolectric-
verified only until the emulator host lands; linux/macos/ios/windows run the
full tier_3 suite here on the primary dev VM like web. The
`web`/`android`/`linux`/`tui`/`macos`/`ios`/`windows` markers deselect this
under a `--client` naming none of them.
"""

import secrets

import pytest

from clients.ws_rpc_anon_client import RpcCallError
from common.auth import register_handled_actor
from conftest import MAIL_PRIMARY_DOMAIN

pytestmark = [
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.linux,
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.tier_3,
]


@pytest.mark.feature("admin-users")
def test_age_verification_knob_rides_the_section_save_and_bites(registration_posture_admin_app):
    """The Registration section's age require-knob
    (`admin-users-registration-age-verification-toggle`, family-safety.md § App
    surface → *Age-band surfaces*, D5+D6): off on a fresh nest; flipping it and
    pressing the section's one Save persists it (`fauna.admin.set_age_verification_required`,
    read back from `fauna.setup.status` — the controls re-seed from the nest);
    and it BITES on the open posture — a stranger carrying no verified age
    claim is refused at `fauna.account.register` with the wire-stable
    `age_verification_required` code (public-mode.md § Age at registration).
    Flipping it back off readmits strangers. On the DEDICATED posture nest,
    because the knob is deployment-wide state that would break every later
    registration on the shared one.
    """
    app = registration_posture_admin_app
    nest = app.registration_posture_nest

    # Baseline: open posture, no knob — a stranger registers.
    _register_stranger(nest)

    app.admin.navigate_users()
    assert app.admin.age_verification_state() == "off", (
        f"a fresh nest's knob reads off: {app.admin.age_verification_state()!r}"
    )
    app.admin.set_age_verification(True)
    app.admin.save_registration()
    assert app.admin.users_action_error_text() in ("", None), (
        f"saving the knob must not error: {app.admin.users_action_error_text()!r}"
    )
    app.admin.navigate_users()
    assert app.admin.age_verification_state() == "on", (
        "the saved knob must survive the page's re-read (persisted, not local): "
        f"{app.admin.age_verification_state()!r}"
    )

    # It bites: the same self-service call now refuses a claim-less signup.
    with pytest.raises(RpcCallError) as refused:
        _register_stranger(nest)
    assert refused.value.code == "fauna.account.age_verification_required", (
        f"expected the age-verification refusal, got {refused.value.code!r}: {refused.value}"
    )

    # And off again — one more save, no half-saved section.
    app.admin.set_age_verification(False)
    app.admin.save_registration()
    app.admin.navigate_users()
    assert app.admin.age_verification_state() == "off"
    _register_stranger(nest)


def _register_stranger(nest) -> dict:
    """Self-register a brand-new actor over `fauna.account.register` — the exact
    path a stranger takes. Raises on refusal, which is what the closed-posture
    assertions catch."""
    return register_handled_actor(
        nest["port"],
        handle="stranger" + secrets.token_hex(3),
        domain=MAIL_PRIMARY_DOMAIN,
    )


@pytest.mark.feature("admin-users")
def test_admin_reads_back_the_posture_the_nest_is_running(registration_posture_admin_app):
    """The section shows the *nest's* posture, not a client-side default.

    The fixture's nest is seeded `open`, while a fresh nest's default is `closed`
    — so a picker that rendered a hardcoded default would read `closed` here and
    fail. This is the read-back half of the section (`fauna.setup.status`
    → `registration_mode`).
    """
    app = registration_posture_admin_app

    app.admin.navigate_users()

    assert app.driver.is_visible("admin-users-registration-section"), (
        "the registration section must render on the admin-users page. "
        f"error: {app.admin.users_action_error_text()!r}"
    )
    assert app.admin.registration_mode() == "open", (
        "the picker must show the posture the nest is actually running (seeded "
        f"`open`), got {app.admin.registration_mode()!r} — a client-side default "
        "would read `closed`. "
        f"error: {app.admin.users_action_error_text()!r}"
    )


@pytest.mark.feature("admin-users")
def test_admin_closes_registration_from_their_client_and_a_stranger_is_refused(
    registration_posture_admin_app,
):
    """The whole point: an admin changes the posture from their client, with no
    restart and no config file, and it bites on the next stranger.

    Ordered so the UI is load-bearing in *both* directions. The nest starts `open`
    and a stranger registers — that baseline is the negative control, without which
    the refusal below would also pass on a nest that had simply never been touched
    (a fresh nest defaults `closed`). Only after the UI write does the same call
    fail.
    """
    app = registration_posture_admin_app
    nest = app.registration_posture_nest

    # Baseline: the nest genuinely admits strangers right now.
    _register_stranger(nest)

    app.admin.navigate_users()
    app.admin.set_registration_mode("invite_required")
    app.admin.save_registration()

    assert app.admin.users_action_error_text() in ("", None), (
        f"saving the posture must not error: {app.admin.users_action_error_text()!r}"
    )
    # The controls re-seed from `fauna.setup.status` after the save, so this reads
    # the *persisted* posture — proving the write reached the nest, not just the
    # local select.
    assert app.admin.registration_mode() == "invite_required", (
        "the saved posture must survive the page's re-read, got "
        f"{app.admin.registration_mode()!r}"
    )

    # And it bites, live — same call, same nest process, no restart.
    #
    # Assert the wire-stable code, not a substring: `signature_failed` (a
    # handle-domain mismatch) and `free_limit_reached` are both plausible refusals
    # of this same call, and either would make a looser assertion pass for a reason
    # that has nothing to do with the posture the admin just set.
    with pytest.raises(RpcCallError) as excinfo:
        _register_stranger(nest)
    assert excinfo.value.code == "fauna.account.invite_required", (
        "a stranger with no invite code must be refused *because an invite is now "
        f"required*, once the admin sets invite-required from their client; got "
        f"{excinfo.value.code!r}"
    )


def test_every_registration_control_is_reachable_on_the_hub(registration_posture_admin_app):
    """A user can actually get at every control of the Registration section.

    Deliberately NOT `feature`-marked: this is a layout regression guard, not a
    catalog outcome (`features-lint` rule 3 would demand a coverage-contract line
    for it, and the outcomes there are hand-authored). The outcome it protects is
    the one the tests around it witness — the admin sets the posture from their app.

    The section is one mode select, the free-tier ceiling, the age require-knob
    and the ONE Save that commits all three (`admin.md` § 2) — so a control a
    user cannot reach strands the whole gesture. Every other test in this file
    drives these controls by id, and an id-driven write reaches a control the
    window has clipped to nothing (UIA `Invoke`/`ValuePattern` need no pixels), so
    a layout that overflows its column — windows' horizontal row did, at the
    600-DIP minimum window, hiding the ceiling input, the toggle and Save — stays
    green everywhere except a `wait_for`. This test IS that `wait_for`, on every
    control, with all the failures in one message (e2e rule 6) so a layout defect
    names each stranded control rather than only the first.

    `wait_for` scrolls the page's vertical scroller, so a control merely below the
    fold passes; only one no vertical scroll can reach (a horizontal overflow, an
    unrendered section) fails.

    The age toggle is checked wherever an app has RENDERED it (`count > 0`): an app
    that has not built the knob yet (web, 2026-09-26) is
    `test_age_verification_knob_rides_the_section_save_and_bites`'s red, and a second
    red for the same parity gap here would say nothing about layout. A toggle that is
    in the tree but unreachable — the windows overflow's exact shape, `count=1,
    visible=False` — still fails.
    """
    app = registration_posture_admin_app

    app.admin.navigate_users()
    # The section paints after `navigate_users` returns (see the ceiling test
    # below), so gate on its first control with the long budget; once the section
    # is up the rest are immediate and a failure needs only a short one.
    app.driver.wait_for("admin-users-registration-mode-select", timeout=30)

    controls = [
        "admin-users-max-free-users-input",
        "admin-users-registration-save-button",
    ]
    if app.driver.count("admin-users-registration-age-verification-toggle") > 0:
        controls.append("admin-users-registration-age-verification-toggle")

    stranded = []
    for control_id in controls:
        try:
            app.driver.wait_for(control_id, timeout=10)
        except TimeoutError as e:
            stranded.append(str(e))
    assert not stranded, (
        "Registration controls a user cannot reach:\n  " + "\n  ".join(stranded)
    )


@pytest.mark.feature("admin-users")
def test_the_free_tier_ceiling_saves_alongside_the_mode(registration_posture_admin_app):
    """Mode + ceiling are one decision, saved by one button (`admin.md` § 2).

    The ceiling is *orthogonal* to the mode — it applies regardless of posture — so
    this asserts both values survive the same save, and that blank round-trips as
    "no cap" rather than as 0.
    """
    app = registration_posture_admin_app

    app.admin.navigate_users()
    # `navigate_users` waits on the user rows and falls through on a slow load,
    # so it does not prove the Registration section painted; a one-shot read of
    # the field raced it (LookupError, the 2026-09-22 linux sweep's first red).
    app.driver.wait_for("admin-users-max-free-users-input", timeout=30)
    assert app.admin.max_free_users() == "", (
        "the fixture's nest sets no ceiling, so the field must start blank (= no "
        f"cap), got {app.admin.max_free_users()!r}"
    )

    # The cap counts EVERY free-tier account including the admin's own, so 2 is
    # "the admin plus one more" — not "two more".
    app.admin.set_registration_mode("open")
    app.admin.set_max_free_users("2")
    app.admin.save_registration()

    assert app.admin.users_action_error_text() in ("", None), (
        f"saving mode + ceiling must not error: {app.admin.users_action_error_text()!r}"
    )
    assert app.admin.max_free_users() == "2", (
        f"the ceiling must survive the re-read, got {app.admin.max_free_users()!r}"
    )
    assert app.admin.registration_mode() == "open", (
        "the mode saved by the same button must survive too, got "
        f"{app.admin.registration_mode()!r}"
    )

    # Blank clears it — the wire value is None (no cap), not 0.
    app.admin.set_max_free_users("")
    app.admin.save_registration()
    assert app.admin.max_free_users() == "", (
        f"blank must clear the ceiling, got {app.admin.max_free_users()!r}"
    )
