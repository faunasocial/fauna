"""Recipient picker resolves across rails; rail locks after first chip."""
import pytest

from helpers import real_rail_control
from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until
from tests.api import conv_api

# real_conversations: windows registers the REAL FaunaMls rail for every e2e login
# but runs the launch's replica restore — the step that marks the account's
# conversations loaded — only under this marker. Until that lands, a foreign
# domain that does not answer discovery is a failed lookup ("Lookup failed — try
# again"), terminal for the rail chain and never an email fallthrough
# (federation.md § Peer-auth model → Discovery-failure semantics). Every handle
# below sits at a domain no nest serves, so unmarked, windows reads each as an error.
pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


def test_the_real_faunamls_gate_actually_opened(logged_in_app):
    """The module's own positive control: this app really is on the real rail.

    `real_conversations` (module-level above) is a **launch-time** gate on
    windows/macOS/iOS/android and a runtime toggle elsewhere, and on macOS/iOS
    the app publishes no client-side readiness signal at all. So an apple run of
    this module could pass every test below against the MOCK backend and prove
    nothing, which is exactly what the pre-2026-09-21 apple greens are suspected
    of having done: a passing test prints no app log, so the only apple evidence
    the gate ever opened came from a macOS run that *failed*.

    The fixture next door (``real_faunamls_app``) runs this same control as a
    precondition for ITS consumers, but cannot cover this module — see the body.

    This splits the mile rather than leaving "did the real backend run" to a
    human reading a log: the mechanism is testable headlessly, so assert the
    app's own account of itself instead. A green here makes every other green in
    the module a real-backend green.

    **Not ``keypackage_count``.** The obvious nest-side control — alice's key
    packages going non-zero — is unsound in this harness: ``test_user`` is
    session-scoped (conftest: "accumulating state on it is the point"), so in a
    multi-app invocation a non-zero count may have been published by an *earlier*
    app in the same run. A control that another app can satisfy for you is not a
    control. The app's own log cannot be borrowed that way.
    """
    # The mechanism — both stages, the android/web log-reader guards and the
    # budgets — is `helpers/real_rail_control.py`, shared with the
    # `real_faunamls_app` fixture, which runs the SAME control as a precondition
    # for its own consumers. This module cannot inherit the fixture's copy: its
    # tests take `logged_in_app` and reach the real rail through the module-level
    # `real_conversations` marker, never through that fixture. Two call shapes,
    # one control — not two controls to keep in step.
    logged_in_app.conversations.navigate()
    real_rail_control.witness_real_rail(
        logged_in_app.driver, context="this module's own positive control"
    )


def _commit_and_read_class(app, recipient: str) -> str:
    """Type ``recipient``, commit it as a chip, and return the composer's
    prospective room class — the read that names WHICH rail claimed the address.

    ``recipient-resolve-status``'s ``resolved`` cannot: the rail chain ends in the
    email fallthrough, so an address the Fauna rail never claimed still reads
    ``resolved`` (federation.md § Peer-auth model → Discovery-failure semantics,
    case 2: a domain nobody answers for, evidence loaded → email).
    ``recipient-picker-class`` is derived in shared Rust from each committed
    chip's rail (``prospective_room_class``): a Fauna chip reads ``end-to-end``,
    an email chip ``transport-only``. Read with the composer still open —
    sending tears the picker down.
    """
    conv = app.conversations
    state = conv.resolve_recipient(recipient)
    assert state == "resolved", (
        f"{recipient!r} should resolve on some rail, got {state!r}. "
        f"{app.driver.diagnose('recipient-resolve-status')} "
        f"error: {app.error_text()!r}"
    )
    conv.accept_recipient_chip(recipient)
    return wait_until(
        conv.prospective_room_class,
        UI_SETTLE_S,
        diagnose=lambda: app.driver.diagnose("recipient-picker-class"),
    )


@pytest.mark.feature("conversations")
def test_resolves_fauna_handle(logged_in_app, nest_instance, test_user):
    """Typing handle@nest where that nest serves the handle with published MLS
    key packages resolves as Fauna — the FaunaMls rail claims it, not email.

    Red-first proof (2026-09-24, tui): this class assertion against
    ``alice@self-nest.test`` — the address the test typed until then, under a
    bare ``state == "resolved"`` that could not fail — reads ``transport-only``;
    that address is now ``test_unserved_nest_domain_falls_through_to_email``.
    """
    port = nest_instance["port"]
    # A same-nest peer the FaunaMls resolve can actually claim: a handled actor
    # with key packages (zero packages → the rail reports NotFound and the chain
    # falls through to email).
    bob = conv_api.reachable_peer(
        port, nest_instance["admin"]["signing_key"], test_user["actor_id_hex"]
    )
    # The domain a typed `handle@domain` must match to resolve same-nest is the
    # one the nest echoes for its own handles — read it rather than assume it.
    served = conv_api.actor_by_handle(port, test_user, bob["handle"])
    assert served.get("addressable"), (
        f"precondition: the nest should report {bob['handle']!r} addressable "
        f"after its key-package upload, got {served!r}"
    )
    address = f"{bob['handle']}@{served['domain']}"

    room_class = _commit_and_read_class(logged_in_app, address)
    assert room_class == "end-to-end", (
        f"{address!r} is served by the logged-in nest with published key packages, "
        f"so the FaunaMls rail should claim it (end-to-end); got {room_class!r} — "
        "'transport-only' means the chain fell through to email. "
        f"error: {logged_in_app.error_text()!r}"
    )


def test_unserved_nest_domain_falls_through_to_email(logged_in_app):
    """A nest-looking handle at a domain no nest serves resolves as EMAIL, never
    Fauna (federation.md § Discovery-failure semantics, case 2)."""
    room_class = _commit_and_read_class(logged_in_app, "alice@self-nest.test")
    assert room_class == "transport-only", (
        "no nest serves self-nest.test, so the address should fall through to "
        f"the email rail (transport-only); got {room_class!r}"
    )


def test_resolves_email(logged_in_app):
    """Typing user@host where host has no Fauna nest resolves as Email."""
    room_class = _commit_and_read_class(logged_in_app, "alice@plain-email-host.test")
    assert room_class == "transport-only", (
        f"an email recipient should commit on the email rail, got {room_class!r}"
    )

def test_resolves_bluesky_did(logged_in_app):
    """Typing did:plc:... resolves as Bluesky."""
    state = logged_in_app.conversations.resolve_recipient("did:plc:test123")
    # error acceptable if test DID doesn't exist; the assertion only requires a
    # terminal resolve state, never the in-flight "resolving".
    assert state in ("resolved", "error"), (
        f"a bluesky DID should reach a terminal resolve state, got {state!r}. "
        f"{logged_in_app.driver.diagnose('recipient-resolve-status')}"
    )

def test_rail_locks_after_first_chip(logged_in_app):
    """After accepting a Fauna chip, suggestions filter to compatible rails."""
    logged_in_app.conversations.navigate()
    logged_in_app.driver.click("new-conversation-button")
    logged_in_app.driver.fill("recipient-picker-input", "alice@self-nest.test")
    logged_in_app.conversations._wait_resolve()
    logged_in_app.conversations.accept_recipient_chip("alice@self-nest.test")
    # Now type a Bluesky account DID — should not auto-suggest cross-rail
    logged_in_app.driver.fill("recipient-picker-input", "did:plc:other")
    # Implementation: suggestions filter; expect no Bluesky suggestion for rail-locked thread
    assert logged_in_app.driver.count("recipient-picker-suggestion") == 0, (
        "Bluesky suggestion should not appear after Fauna chip locks the rail: "
        f"{logged_in_app.driver.diagnose('recipient-picker-suggestion')}"
    )

@pytest.mark.feature("conversations")
def test_multi_recipient_creates_group(logged_in_app):
    """Adding a second compatible chip surfaces the group-conversation hint."""
    logged_in_app.conversations.navigate()
    logged_in_app.driver.click("new-conversation-button")
    logged_in_app.driver.fill("recipient-picker-input", "alice@self-nest.test")
    logged_in_app.conversations._wait_resolve()
    logged_in_app.conversations.accept_recipient_chip("alice@self-nest.test")
    logged_in_app.driver.fill("recipient-picker-input", "bob@self-nest.test")
    logged_in_app.conversations._wait_resolve()
    logged_in_app.conversations.accept_recipient_chip("bob@self-nest.test")
    # Hint text from i18n: conversations.group_conversation_hint
    assert logged_in_app.driver.is_visible("group-conversation-hint"), (
        "group conversation hint should appear when N>=2 recipients added: "
        f"{logged_in_app.driver.diagnose('group-conversation-hint')}"
    )
