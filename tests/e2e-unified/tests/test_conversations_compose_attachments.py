"""Staged-attachment compose preview + remove — ``dm-compose-attachment-chip``.

The *send* half of conversations attachments (stage → seal → deliver → render
in the sender's echo) is covered by
``test_conversations_attachments_outbound.py``. This module covers the half
that sits entirely in front of it: once a file is staged, the composer must
SHOW it and let the user take it back off before sending.

Until 2026-07-20 no client rendered ``ComposeState.attachments`` at all
(``conversations.md`` § Attachments — "no staged-attachment preview/remove UI
exists on any client yet; windows shows a bare, non-test-ID'd filename text"),
so a user attached a file and got zero feedback that anything staged until the
message sent, and the built ``remove_attachment`` /
``remove_new_thread_attachment`` mutators had no caller anywhere. The IDs are
user-approved (2026-07-20): ``dm-compose-attachment-chip`` (indexed) plus the
sibling ``dm-compose-attachment-remove``, in both ``dm-compose-bar`` and
``dm-compose-form``, mirroring the ``dm-reply-recipient-chip`` /
``dm-reply-recipient-remove`` prior art.

**Why the new-thread composer, and why no real backend.** Staging, chip render
and unstage are pure ``ComposeState`` mechanics — the manager hashes the bytes,
caches them and stages a light ``AttachmentDraft`` — so none of it needs a
bound MLS channel, a real peer, or the real FaunaMls rail. Driving the
new-thread composer (``add_new_thread_attachment`` /
``remove_new_thread_attachment``) therefore exercises the mechanism under test
with no group bootstrap at all, which keeps this test fast and free of the
real-backend marker trap that the outbound module's docstring documents at
length. The inline-reply leg (``add_attachment`` / ``remove_attachment``,
same shared ``DmComposeBar``) needs a bound thread and is covered implicitly by
the outbound module's staging step.

``set_input_files`` stands in for the OS file panel on every native app
(``drivers/http_bridge.py::set_input_files`` — the agent stages against the
same call the real picker's completion handler makes, routed by the patch's
``target`` field); on web it is Playwright's real ``<input type="file">``.

One shared body, per-app markers: apple leads (the IDs landed with its
shared ``FaunaKit/Views/DmComposeBar.swift`` chip row). Every other app
adds its own marker line here as it lifts the same chip — the lift is the
marker, not a new test. tui joined 2026-07-30 (``attachment_elements`` in
``apps/fauna-tui/src/conversations/mod.rs``, shared by both composers); its
``attachment-button`` is a typed-path input rather than a picker, per
``apps/tui.md`` § Declared platform absences 4, and ``set_input_files``
reaches it through the same ``target``-disambiguated compose patch.
"""

from pathlib import Path

import pytest

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def _stage_one_attachment(app):
    """Open the new-thread composer and stage ``FIXTURE_IMAGE`` onto it.

    Returns the staged filename. Fails loudly on a staging error rather than
    letting a dropped command read as "the chip never rendered" 10s later
    (e2e rule 11 — the agent reports staging failures on ``error-message``).
    """
    conv = app.conversations
    driver = app.driver

    conv.navigate()
    driver.click("new-conversation-button")

    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    assert not app.has_error(), f"staging the attachment failed: {app.error_text()}"

    return FIXTURE_IMAGE.name


@pytest.mark.tier_3
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.feature("conversation-attachments")
def test_staged_attachment_shows_a_chip_and_can_be_removed(logged_in_app):
    """Stage → the chip names the file → remove → the chip is gone.

    The remove assertion is the load-bearing half: ``remove_attachment`` /
    ``remove_new_thread_attachment`` shipped built-and-unused on every app,
    so this is the first test anywhere that drives either of them.
    """
    app = logged_in_app
    driver = app.driver
    filename = _stage_one_attachment(app)

    assert driver.count("dm-compose-attachment-chip") == 1, (
        "staging one file must render exactly one staged-attachment chip in the "
        f"composer (error-message: {app.error_text()!r}): "
        f"{driver.diagnose('dm-compose-attachment-chip')}"
    )

    chip = driver.get_text("dm-compose-attachment-chip", index=0)
    assert filename in chip, (
        f"the staged chip must name the file it staged; got {chip!r}, "
        f"expected to contain {filename!r}"
    )

    # Remove it — the built-but-never-called mutator.
    driver.click("dm-compose-attachment-remove", index=0)

    assert driver.count("dm-compose-attachment-chip") == 0, (
        "removing the only staged attachment must drop its chip "
        "(remove_new_thread_attachment did not reach the compose state): "
        f"{driver.diagnose('dm-compose-attachment-chip')}"
    )
    assert not app.has_error(), f"removing the attachment errored: {app.error_text()}"


@pytest.mark.tier_3
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.feature("conversation-attachments")
def test_staged_attachment_chip_shows_its_size(logged_in_app):
    """The chip carries the file's size, not just its name.

    Size comes from the shared ``fauna_core::format::byte_size``
    (``value-formatting.md`` § Byte sizes → "{value} B/KB/MB…"), never a
    per-app hand-roll, so asserting the unit here also pins that the client
    routed through the shared formatter. The fixture is a small PNG, so it
    lands in the ``B``/``KB`` range.
    """
    app = logged_in_app
    driver = app.driver
    _stage_one_attachment(app)

    chip = driver.get_text("dm-compose-attachment-chip", index=0)
    assert any(unit in chip for unit in (" B", " KB", " MB")), (
        "the staged chip must show the attachment's size via the shared "
        f"byte_size formatter; got {chip!r}"
    )
