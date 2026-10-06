"""tier_3 — the co-present **offline share-initiation** affordance, driven
through the app UI (`p2p.md` § Offline share initiation, contract point 1).

The ceremony's whole non-UI stack is proven in Rust — the two-party walk over a
real peer channel lives in `fauna-client-capabilities`'s
`group_ceremony_over_wire` and `group_ceremony_node` integration tests, and the
paint decision has its own tier_1 truth tables. What NONE of those can answer is
the question this file exists for: **is the affordance actually reachable in a
running app**, and does the code it shows a user equal the identity the dial
would prove?

That second half is the load-bearing one. The security of a co-present ceremony
is that two people compare a code out loud; if the app displayed anything other
than its own actor key — a truncation, a different key, a stale value — the
comparison would still *look* like it worked while proving nothing. A unit test
over the element function cannot catch that, because it is handed the identity
it then asserts. Here the code is read out of a live app and checked against the
actor id the nest registered.

⚠ **What this deliberately does NOT assert.** This file is the SINGLE-seat
half: what one device shows, refuses and lets a user back out of before any
counterpart exists. The delivery journey it stops short of — the offer
crossing, the recipient's consent card, admission, and "the recipient's
folders page lists the shared set" — is BUILT and asserted next door in
`test_offline_share_two_seat.py`, green on tui, linux, macos and windows.
(An earlier version of this paragraph called that consent card deliberately
unbuilt, pending a group store. It landed 2026-09-15; the paragraph is
retired rather than reworded so no later reader inherits the stale premise.)

What remains genuinely unasserted anywhere is group-scope **content
transfer**: the group serve door still checks the folder family only, and
group content-kind sealing is design-only
(`account-data-plane.md` § Implementation status today). Bytes are that
slice's journey, not this one's.
"""

import secrets

import pytest

from helpers.app_surface import skip_environment

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui leads this affordance (`p2p.md` § Offline share initiation → build
    # order: "tui leads the affordance"); linux, macos+ios, windows and android
    # joined it as their UI legs landed — 2026-08-20, 2026-08-25/26, 2026-08-27
    # and 2026-09-22 respectively (`p2p.md` § Implementation status today). The
    # file gates on the affordance being RENDERED rather than on the driver
    # type, so a leg needs no edit here beyond its marker.
    #
    # ⚠ The marker IS this file's app list, and a `_SUPPORTED_APPS` beside it
    # would be a SECOND truth. Its two-seat sibling needs one because
    # `ceremony_seats` there builds its own seat set, which a marker alone
    # left hardcoded to tui for a year; here every test takes `logged_in_app`,
    # whose `app` fixture is parametrized over `get_available_apps()` and then
    # narrowed by these very markers.
    #
    # One of the seven is absent on purpose: **web** is a declared absence
    # for this ceremony rather than a lagging leg (`p2p.md` § Wormability walk
    # rule 5 — the same no-QUIC-seam-in-the-wasm-graph reason the transfer
    # plane is absent from web).
    #
    # ⚠ android's positive gate read (`wait_offline_share_begin_enabled`)
    # needs the android bridge's `/element/enabled` route, which it does not
    # serve yet — `HttpBridgeDriver.is_enabled`'s own docstring. Until it does,
    # the act-button test goes RED on android rather than passing vacuously,
    # which is the honest failure: the marker below is the leg's claim, and a
    # red names what is still missing.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
]


def _skip_no_affordance() -> None:
    """The section is absent because this nest does not advertise `p2p-share`
    — wormability rule 7's brake refusing to let a listener bind, which is the
    CORRECT behaviour rather than a gap.

    Declared `skip_environment` and not `skip_unbuilt` deliberately: no app
    work makes this run here, only a nest that advertises the capability.

    That classification is what keeps the file honest as the trickle-down
    grows, and it rests on ONE invariant: **every app marked above has the
    affordance built** (`p2p.md` § Implementation status today). So a `False`
    from `offline_share_available()` cannot mean "this app has not lifted it"
    for any app that reaches here — only the nest can be the cause. Adding a
    marker for an app whose UI leg has NOT landed would silently convert a
    real gap into an environment skip, which is convention 7's own failure
    mode (a capability table answering "not implemented" for an app it never
    heard of). android was the live case until its leg landed (2026-09-22):
    the marker joined in the same change as the Compose glue, never before it.
    """
    skip_environment(
        "this nest does not advertise the p2p-share capability, so no "
        "ceremony listener may bind (p2p.md § Wormability posture, rule 7)"
    )



@pytest.mark.feature("offline-sharing")
def test_the_compare_code_the_app_shows_is_this_actors_own_key(
    logged_in_app, test_user
):
    """The in-person compare is the ceremony's only defence against a
    man-in-the-middle, and it rests entirely on this equality."""
    app = logged_in_app
    b = app.backups

    b.navigate_folders()
    if not b.offline_share_available():
        _skip_no_affordance()

    b.open_offline_share()
    shown = b.offline_share_own_code()
    # The code LEADS with this actor's key, and may carry addressing after it
    # (`p2p.md` § Offline share initiation → contract point 1, *The compare
    # code carries the addressing*). The identity half is what a human
    # compares, so that is what this asserts — verbatim at the front, with any
    # suffix separated by the code's own separator rather than run together.
    assert shown == test_user["actor_id_hex"] or shown.startswith(
        test_user["actor_id_hex"] + "-"
    ), (
        "offline-share-own-code must LEAD with this actor's key verbatim — the "
        "whole security of the co-present ceremony is that a human compares "
        f"that half. shown={shown!r} actor={test_user['actor_id_hex']!r}; "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("offline-sharing")
def test_the_act_button_refuses_an_unusable_code_and_accepts_a_real_one(
    logged_in_app, test_user
):
    """Begin is gated on a code that parses, and — the part worth a journey —
    on it not being THIS device's own code. A self-dial would mint a scope
    shared with nobody, and is far likelier to be a mis-paste than an intent."""
    app = logged_in_app
    b = app.backups

    b.navigate_folders()
    if not b.offline_share_available():
        _skip_no_affordance()

    b.open_offline_share()

    # Nothing typed: the act is unreachable, silently.
    assert not b.offline_share_begin_enabled(), (
        "offline-share-begin-button must start disabled — there is no "
        f"counterpart yet; error={app.error_text()!r}"
    )

    # Something that is not a key at all.
    b.type_offline_peer_code("not-a-code")
    assert not b.offline_share_begin_enabled(), (
        "a malformed compare code must leave Begin disabled rather than be "
        f"repaired into something; error={app.error_text()!r}"
    )

    # The user's OWN code — the mis-paste this refusal exists for.
    b.type_offline_peer_code(test_user["actor_id_hex"])
    assert not b.offline_share_begin_enabled(), (
        "Begin must refuse this device's own compare code: a self-dial mints "
        f"a scope shared with nobody; error={app.error_text()!r}"
    )

    # A real counterpart key — any key that is not ours. Generated rather than
    # fixed so the assertion cannot pass by matching a hard-coded value.
    other = secrets.token_hex(32)
    assert other != test_user["actor_id_hex"]
    b.type_offline_peer_code(other)
    # Waits on the gate's STATE, not the instant after the typing returns — the
    # refusals above keep the bare read, which a wait cannot serve.
    b.wait_offline_share_begin_enabled()


@pytest.mark.feature("offline-sharing")
def test_the_two_panels_are_exclusive_and_cancel_returns_to_the_entry(
    logged_in_app,
):
    """The two roles are opposite ends of one ceremony: a device mid-initiation
    is not also awaiting one. Cancel must always be a way out — an in-flight
    panel with no exit is a trap."""
    app = logged_in_app
    b = app.backups

    b.navigate_folders()
    if not b.offline_share_available():
        _skip_no_affordance()

    # ⚠ The panel's lower controls (status, Begin/Expect, Cancel) sit below the
    # peer-code field, so on a window shorter than the open panel they are one
    # scroll away — and windows' `is_visible` is UIA `!IsOffscreen`, so a correct
    # panel reads False for them (measured on Windows, 2026-09-21: count=1, a 0x0
    # rect, until scrolled) while `open_offline_share()` only pins the panel's top
    # (`offline-share-own-code`). The positive reads therefore use
    # `is_visible_scrolled`, which asks what they mean — "is it rendered" — and
    # degrades to a plain `is_visible` where a driver cannot scroll. The NEGATIVE
    # reads use `is_absent` (tree membership; `count == 0` on windows) and so
    # depend on no scroll position at all: they used to read a bare `is_visible`
    # and were honest only by adjacency to a scrolled positive read — an
    # argument, not a measurement (convention 6's rider).

    # Initiator panel: its own act button, never the other role's.
    b.open_offline_share()
    assert app.driver.is_visible_scrolled("offline-share-begin-button"), (
        f"the initiator panel owes Begin; error={app.error_text()!r}; "
        f"{app.driver.diagnose('offline-share-begin-button')}"
    )
    assert app.driver.is_absent("offline-receive-expect-button"), (
        "the initiator panel must not offer the recipient's act: "
        f"{app.driver.diagnose('offline-receive-expect-button')}"
    )
    assert app.driver.is_visible_scrolled("offline-share-status"), (
        "an open panel always reports where the ceremony has got to; "
        f"{app.driver.diagnose('offline-share-status')}"
    )

    # Out, and back to the two entry buttons.
    b.cancel_offline_share()
    assert b.offline_share_available()
    assert app.driver.is_absent("offline-share-own-code"), (
        "a closed panel must not leave the compare code on screen: "
        f"{app.driver.diagnose('offline-share-own-code')}"
    )

    # Recipient panel: the mirror image.
    b.open_offline_receive()
    assert app.driver.is_visible_scrolled("offline-receive-expect-button"), (
        f"the recipient panel owes the receive act; error={app.error_text()!r}; "
        f"{app.driver.diagnose('offline-receive-expect-button')}"
    )
    assert app.driver.is_absent("offline-share-begin-button"), (
        "the recipient panel must not offer the initiator's act: "
        f"{app.driver.diagnose('offline-share-begin-button')}"
    )
    b.cancel_offline_share()
