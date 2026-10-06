"""Conversation attachments: a REAL UI-driven send stages + delivers a file.

The compose-side half of ``conversations.md`` § Attachments outbound
(`attachment-button` -> file picker -> `manager.add_attachment`/
`add_new_thread_attachment`) was, until 2026-07-20, unwired on every app
except windows -- `attachment-button` existed in the DOM/tree everywhere but
had no click handler / no registered callback, so no e2e could prove the real
send-with-attachment path (`test_conversations_attachments.py` covers the
INBOUND/render half only, via the ``inject_inbound_for_test`` seam).

Both tests below drive the real UI end to end: a real click on
``attachment-button`` stages a real file via ``ConversationsManager
.add_attachment``/``add_new_thread_attachment``, then a real click on
``dm-send-button`` calls the real ``manager.send()``. The manager builds the
sender's own echo message from the SAME resolved attachment bytes regardless
of which rail backend handles the wire send (`manager.rs`
``sent_echo_attachments``) -- never trust a helper that skips the click, per
the project's "coverage claims are load-bearing" rule; neither test does.

Web: a real ``<input type="file">`` (the same shape the feed composer's
``compose-file`` already uses) -- Playwright's ``set_input_files`` sets it
directly over CDP, no OS file-chooser dialog involved. Web's e2e DEFAULT is the
FaunaMls MOCK backend, so the test-helper `create_mls_group` thread (a synthetic
peer, no real key package) sends fine -- but the mock never uploads a blob, so
that leg proves the UI and nothing about the wire. Web therefore has a SECOND
leg (``test_web_real_faunamls_send_with_attachment_renders_in_sender_echo``)
which opts into the real rail via ``real_faunamls_app`` and bootstraps a real
peer exactly as the linux leg does; it is the only test anywhere that drives
web's own ``WsConversationsRpc::blob_put`` against a real nest, and its absence
is what let that call's raw-body bug survive from the strict-blob flip to
2026-07-31.

Linux: a native GTK4 file-chooser portal dialog can't be driven by AT-SPI (or
any headless-CI-safe automation), so ``set_input_files`` goes through the
same compose-state-protocol bypass feed's ``compose-file`` already uses --
the test agent stages the file directly against
``ConversationsManager::add_attachment``/``add_new_thread_attachment`` (the
exact call the real ``.on_attach`` GTK-dialog callback in
``views/conversations/detail.rs`` makes once a file is picked), so everything
from "file selected" onward -- staging, send, seal, sender echo -- runs the
real production path; only the OS-level file-chooser widget itself (which is
a separate compositor-drawn process outside the app's own accessibility
tree, same as feed's) is bypassed.

Unlike web, linux always wires the REAL ``FaunaMlsBackend`` at login, so
`create_mls_group`'s synthetic peer (a placeholder all-zero ``actor_id``) can
never bootstrap a real MLS group: the first real send hits
`FaunaMlsBackend::bootstrap_group`, which fetches the peer's real key
package -- there is none, and for an `@self-nest.test`-style foreign handle
the fetch even tries (and fails) a federation DNS lookup. Confirmed
empirically: driving `create_mls_group(["bob@self-nest.test"])` + a real
click send on linux fails nest-side with `federation keypackage fetch
(channel): resolve peer nest_id: ... failed to lookup address information`.
So the linux leg follows ``test_fauna_mls_real_roundtrip.py``'s established
real-backend pattern instead: a genuine same-nest peer with a real minted key
package, bootstrapped via ``real_resolve_send_new`` (fixture setup --
precondition-arranging, not the mutation under test; e2e rule 8 carve-out (b)
-- this first send establishes the bound channel so the SECOND send, the
actual attach+send under test, never needs to bootstrap and hits
``encode_body``/``blob_put`` directly).

macOS / iOS: ``attachment-button`` opens a real SwiftUI ``.fileImporter`` OS
panel (landed 2026-07-20), which -- exactly like linux's GTK portal -- lives
outside the app's own accessibility tree and cannot be driven in-process, so
it registers no automation ``activate`` and ``set_input_files`` goes through
the same compose-state seam: the test agent's ``attachConversationFile``
routes on the patch's ``target`` field to ``ConversationsVM.attachNewThreadFile``
/ ``attachFile`` -> ``manager.add_new_thread_attachment``/``add_attachment``,
the identical calls the real ``.fileImporter`` completion handler makes. Only
the OS panel widget is bypassed; staging, send, seal and sender echo are the
real production path.

⚠ **The apple legs carry ``@pytest.mark.real_conversations`` and MUST run in
their own pytest invocation** (``conftest.py``'s ``real_faunamls_app``
docstring): apple has NO client-side "is the real backend up" signal, so an
apple test consuming that fixture *without* the marker silently runs against
the MOCK backend and can pass while proving nothing. The marker is per-test,
NOT module-level, deliberately: ``_apply_real_conversations_env`` is
session-wide, and a module-level mark would tag the web/linux legs too. It is
inert for them today (the helper is only called from the ios/macos/windows/
android branches of ``_build_app_config``, never linux/web/tui) and a
``--client linux|web`` run deselects the apple legs before
``request.session.items`` is read -- but keeping the mark on exactly the tests
that need it is what stops that inertness from becoming load-bearing. The
apple legs additionally assert alice's own key-package count went non-zero
before sending: publishing login-time KeyPackages is something only the REAL
``start_receive_loop`` does, so that assertion is the anti-mock guard the
missing readiness signal would otherwise leave absent -- a mock-backend run
fails there loudly instead of greening.

Tui: structurally linux's leg (same real ``FaunaMlsBackend``, same
compose-state-protocol staging) with one difference worth knowing -- tui's
``attachment-button`` is not a bypassed OS dialog at all. It is a real typed-path
``input_commit`` element (``apps/tui.md`` § Declared platform absences 4:
"Drag-and-drop and OS file pickers -- replaced by path entry with completion and
a file-browser widget"), so ``set_input_files``' compose patch fills and commits
*the production control itself* rather than standing in for an undrivable widget.
The tui leg is therefore the only native leg with no bypass in it.

Windows: structurally linux's leg (same real ``FaunaMlsBackend`` bootstrap
pattern, same compose-state-protocol staging via `set_input_files` --
``test_conversations_compose_attachments.py`` already proves that seam for
windows). Windows is a LAUNCH-TIME real-backend gate rather than linux/tui's
runtime toggle, so the leg carries ``@pytest.mark.real_conversations``
(``real_faunamls_app``'s own docstring: a windows test consuming that fixture
without the marker raises a legible readiness-poll timeout rather than
silently passing against the mock, unlike macOS/iOS).

Android: ``attachment-button`` launches a real ``GetContent()`` picker -- an OS
activity outside the app's own Compose tree, undrivable in-process like
linux's portal -- whose callback strips EXIF and calls the shared
``manager.addAttachment``/``addNewThreadAttachment``. ``set_input_files`` goes
through ``TestAgent.kt``'s ``compose.file[attachment-button]`` arm, which reads
the open composer off the shared manager's snapshot (an active new-thread
compose, else the selected thread) -- linux's and apple's precedence -- and makes
those same calls. The legs are windows-shaped (launch-time
``real_conversations`` gate, so they share windows' bodies). Three things stand
between them and a recorded run, none in this module: android's run venue
(``docs/goal/architecture/testing.md`` § Default app and nest mode ->
*Android's run venue*, an open user decision); an app-log reader on the
android driver, without which ``real_faunamls_app``'s real-rail control takes
its declared ``skip_unbuilt`` (``helpers/real_rail_control.py``); and a first
run of the file transport on a device -- the agent reads ``file`` on the
device's own filesystem, so ``drivers/android.py``'s ``set_input_files`` carries
the picked file's bytes over the bridge (``POST /input-file``) and hands the
agent the device path the bridge wrote (``test_android_driver_input_file.py``
pins the driver half; the Kotlin half has never run).

**The blob-GC family (outcome 5) is deliberately ONE body for all seven
columns**, unlike the sender-echo legs above: what those legs differ over is
how each app REACHES the send, and the GC witness sits entirely downstream of
that, on a hold written once in shared Rust and pinned by the nest. See
``_conversation_attachment_survives_a_blob_gc_sweep``'s own docstring.
"""

import time
from pathlib import Path

import pytest

from helpers.budgets import MLS_HANDSHAKE_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from tests.api import conv_api

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def _assert_apple_real_backend_is_live(port, test_user):
    """Anti-mock guard for the apple legs (see module docstring): apple's real
    backend is a LAUNCH-TIME gate with no readiness query, and only the real
    ``start_receive_loop`` publishes alice's login-time KeyPackages. If the
    ``real_conversations`` marker failed to take and the MOCK backend is live,
    the caller fails loudly here instead of greening against a mock.

    Shared by every apple leg in this module — the sender-echo pair and the
    blob-GC pair — because the guard belongs to the launch gate, not to any one
    assertion built on top of it.
    """
    deadline = time.time() + 60.0
    alice_kps = 0
    while time.time() < deadline:
        alice_kps = conv_api.keypackage_count(port, test_user, test_user["actor_id_hex"])
        if alice_kps > 0:
            break
        time.sleep(1.0)
    assert alice_kps > 0, (
        "alice published no key packages -- the REAL ConversationsSession never "
        "started, so this run is against the MOCK backend and proves nothing. "
        "Check the real_conversations marker reached _apply_real_conversations_env "
        "(the module must run in its own pytest invocation)."
    )


@pytest.mark.tier_3
@pytest.mark.web
@pytest.mark.feature("conversation-attachments")
def test_web_real_send_with_attachment_renders_in_sender_echo(logged_in_app):
    app = logged_in_app
    conv = app.conversations
    driver = app.driver

    thread_id = conv.create_mls_group(["bob@self-nest.test"])
    assert thread_id, "fixture group thread must materialize + open"

    body = "here is the file"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))

    driver.click("dm-send-button")

    # The send is an async RPC (mock backend, but still awaited through the
    # same wasm plumbing a real rail uses), so the sender's own echo message
    # lands on the next snapshot tick rather than synchronously with the click
    # (e2e action-layer convention: poll, don't assume-once).
    deadline = time.time() + 10.0
    while time.time() < deadline:
        if driver.count("dm-attachment-image") >= 1:
            break
        time.sleep(0.2)

    assert driver.count("dm-attachment-image") >= 1, (
        "a real send with a staged attachment must render dm-attachment-image "
        f"in the sender's own echo message: {driver.diagnose('dm-attachment-image')}"
    )
    assert driver.count("dm-message-text") >= 1, "the sent body must also render"
    assert body in driver.get_text("dm-message-text", index=0)


@pytest.mark.tier_3
@pytest.mark.web
@pytest.mark.feature("conversation-attachments")
def test_web_real_faunamls_send_with_attachment_renders_in_sender_echo(
    real_faunamls_app, nest_instance, test_user,
):
    """The web leg over the REAL FaunaMls rail — the only shape that exercises
    web's own ``WsConversationsRpc::blob_put`` against a real nest.

    The mock-backed web test above cannot reach it: web is the one app whose
    e2e default is the MOCK FaunaMls backend, and the mock never uploads a
    blob. That gap hid a real product bug for eleven days -- web's ``blob_put`` posted a raw
    ``Uint8Array`` body while the nest's blob verifier
    (``bins/fauna-nest/src/blob_routes.rs::upload_blob``) has required
    ``multipart/form-data`` with ``sidecar`` + ``bytes`` parts since the strict
    flip, so **every real attachment send from web failed nest-side with a 400**
    (the native twin had the identical bug until 2026-07-20; fixed here
    2026-07-31 by routing both through one multipart helper per target).

    This assertion is a genuine gate on that, not a render check that would pass
    either way: ``ConversationsManager::send`` appends the sender's own echo
    **only** on ``Ok`` from ``backend.send`` (``manager.rs`` -- the
    ``sent_echo_attachments`` branch lives inside the ``Ok(outcome)`` arm), and
    ``FaunaMlsBackend::encode_body`` propagates a failed ``blob_put`` with
    ``?``. A 400 upload therefore yields NO echo message at all, so
    ``dm-attachment-image`` never appears. Verified empirically red-before /
    green-after on identical test code.

    Structurally the linux leg below, with linux's file-chooser bypass swapped
    for web's real ``<input type="file">`` (Playwright sets it over CDP).
    """
    app = real_faunamls_app
    conv = app.conversations
    driver = app.driver

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Fixture setup (not the mutation under test -- e2e rule 8 carve-out (b)):
    # a real same-nest peer with a real minted key package, then bootstrap a
    # bound 1:1 channel with them. That first send is what creates the MLS
    # group, so the SECOND send -- the attach+send under test -- never needs to
    # bootstrap and goes straight to encode_body -> blob_put.
    bob = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    conv.real_resolve_send_new(bob["actor_id_hex"], "hi bob")

    def _bootstrapped_thread():
        for t in conv.list_threads():
            if t.rail == "FaunaMls" and "hi bob" in (t.snippet or ""):
                return t
        return None

    thread = wait_until(
        _bootstrapped_thread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"threads={[(t.rail, t.snippet) for t in conv.list_threads()]}",
    )
    conv.open_thread_by_id(thread.thread_id)

    # The mutation under test: a real UI attach + send on the now-bound thread.
    body = "here is the file"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    driver.click("dm-send-button")

    # Read the app's own error element before the main assertion so a failed
    # upload diagnoses itself rather than presenting as a bare missing element
    # (convention 6). A 400 from blob_put surfaces here as the send's error.
    def _echo_rendered():
        return driver.count("dm-attachment-image") >= 1

    wait_until(
        _echo_rendered,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"error-message={app.error_text()!r} "
            f"messages={driver.count('dm-message-text')} "
            f"{driver.diagnose('dm-attachment-image')}"
        ),
    )

    texts = [
        driver.get_text("dm-message-text", index=i)
        for i in range(driver.count("dm-message-text"))
    ]
    assert any(body in t for t in texts), f"sent body not found in any rendered message: {texts}"


def _real_send_with_oversized_attachment_surfaces_on_error_message(
    app, nest_instance, test_user, tmp_path,
):
    """Shared body for every app's leg of "a file too large to send is refused
    with a message".

    A REAL backend rejection -- not the ``inject_send_failure_for_test`` seam
    every other compose-error test drives -- must surface on ``error-message``
    too. Proven on web/linux/tui in one pass -- not web-only, since none of
    the three has a prior test proving a genuine (non-injected) send rejection
    surfaces.

    **Why ONE body rather than one per app**, the same reason the blob-GC
    sweep's shared body gives below: the refusal is not app glue. The 10 MiB
    cap is the nest's own ``RequestBodyLimitLayer``, the rejection travels back
    through ``ConversationsManager::send``'s ``Err`` arm into the shared
    ``ComposeState.send_state = Failed{reason}``, and every column paints that
    one field on ``error-message``. From the staged file onward the path is
    shared Rust and the nest; only the staging gesture differs per app.

    ``actions/__init__.py::error_text()`` reads the state protocol first and
    short-circuits to ``""`` whenever ``messages`` exists with ``error: null``,
    WITHOUT ever reading the ``error-message`` element -- so a prior red-probe
    observation of ``app.error_text() == ''`` during a broken-web-``blob_put``
    experiment was inconclusive by construction, not proof any client stays
    silent on a real failure. This test reads the element directly
    (``driver.is_visible``/``driver.get_text``) to settle it for real, with no
    code mutation and no injection seam: an attachment large enough to exceed
    the nest's blob-upload body cap (`/api/v1/blob` carries no per-route
    ``DefaultBodyLimit`` override, so the global 10 MiB
    ``RequestBodyLimitLayer`` -- ``bins/fauna-nest/src/lib.rs`` -- refuses it
    before the handler's own documented cap ever gets a look) is a genuinely
    real rejection a real nest issues on a real multipart POST, over whichever
    transport each app's own `blob_put` uses (wasm `gloo-net` on web,
    `fauna_nest_http` natively on linux/tui).

    Structurally the success-case siblings above with the attachment swapped
    for an oversized one and the assertion flipped: a real rejection must
    render on ``error-message`` (this test) and must NOT render a sender echo
    (asserted below), exactly the inverse of ``ConversationsManager::send``
    appending the echo only inside its ``Ok`` arm.
    """
    conv = app.conversations
    driver = app.driver

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Fixture setup (not the mutation under test -- e2e rule 8 carve-out (b)):
    # bootstrap a bound 1:1 channel exactly as the sibling test above does, so
    # the send under test goes straight to encode_body -> blob_put.
    bob = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    conv.real_resolve_send_new(bob["actor_id_hex"], "hi bob")

    def _bootstrapped_thread():
        for t in conv.list_threads():
            if t.rail == "FaunaMls" and "hi bob" in (t.snippet or ""):
                return t
        return None

    thread = wait_until(
        _bootstrapped_thread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"threads={[(t.rail, t.snippet) for t in conv.list_threads()]}",
    )
    conv.open_thread_by_id(thread.thread_id)

    assert not driver.is_visible("error-message"), (
        "a freshly opened bootstrapped thread must show no error"
    )

    # The mutation under test: a real UI attach + send of an attachment large
    # enough that the nest's real blob-upload route genuinely refuses it -- a
    # real rejection, not `inject_send_failure_for_test`.
    oversized = tmp_path / "oversized-attachment.bin"
    oversized.write_bytes(b"\0" * (15 * 1024 * 1024))

    body = "here is a file too big to accept"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(oversized))
    driver.click("dm-send-button")

    # Read the app's own error element directly -- NOT via `app.error_text()`,
    # whose state-protocol shortcut is exactly what made the original red-probe
    # observation inconclusive (see docstring above).
    wait_until(
        lambda: driver.is_visible("error-message"),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"error_text={app.error_text()!r} "
            f"dm-attachment-image count={driver.count('dm-attachment-image')} "
            f"{driver.diagnose('error-message')}"
        ),
    )

    shown = driver.get_text("error-message")
    assert "conversations.unified" not in shown, (
        f"the i18n key must be resolved through the app's pipeline, not painted raw: {shown!r}"
    )
    assert "send" in shown.lower(), (
        f"the resolved text must be the send-failure template ({shown!r})"
    )
    # A real rejection must never let the sender's own echo render either --
    # `ConversationsManager::send` only appends it inside the `Ok` arm.
    assert driver.count("dm-attachment-image") == 0, (
        "a genuinely rejected upload must not render a sender echo "
        f"(got {driver.count('dm-attachment-image')})"
    )


@pytest.mark.tier_3
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("conversation-attachments")
def test_real_faunamls_send_with_oversized_attachment_surfaces_on_error_message(
    real_faunamls_app, nest_instance, test_user, tmp_path,
):
    """The runtime-toggle trio's leg — web, linux and tui flip the real
    ``FaunaMlsBackend`` live on the session-cached app, so no
    ``real_conversations`` marker is involved and the three share one collected
    test, exactly as the blob-GC sibling below does.
    """
    _real_send_with_oversized_attachment_surfaces_on_error_message(
        real_faunamls_app, nest_instance, test_user, tmp_path
    )


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.windows
@pytest.mark.feature("conversation-attachments")
def test_windows_real_send_with_oversized_attachment_surfaces_on_error_message(
    real_faunamls_app, nest_instance, test_user, tmp_path,
):
    """Windows' leg. Windows is a LAUNCH-TIME real-backend gate rather than the
    trio's runtime toggle, so it needs ``real_conversations`` and therefore its
    own collected test (``real_faunamls_app``'s docstring; a windows test
    consuming that fixture without the marker raises a legible readiness-poll
    timeout, never a silent mock pass). Staging is the same
    compose-state-protocol seam its sender-echo and blob-GC legs already prove,
    and the refusal it must paint is the shared ``send_state = Failed{reason}``
    ``ConversationsPage`` already mirrors onto ``error-message``.
    """
    _real_send_with_oversized_attachment_surfaces_on_error_message(
        real_faunamls_app, nest_instance, test_user, tmp_path
    )


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.android
@pytest.mark.feature("conversation-attachments")
def test_android_real_send_with_oversized_attachment_surfaces_on_error_message(
    real_faunamls_app, nest_instance, test_user, tmp_path,
):
    """Android's leg — windows' launch-gate shape, staged through
    ``TestAgent.kt``'s ``compose.file[attachment-button]`` arm (its sender-echo
    leg below says how). The refusal it must paint is the shared
    ``send_state = Failed{reason}`` ``ConversationDetailScreen`` already mirrors
    onto ``error-message``.
    """
    _real_send_with_oversized_attachment_surfaces_on_error_message(
        real_faunamls_app, nest_instance, test_user, tmp_path
    )


@pytest.mark.tier_3
@pytest.mark.linux
@pytest.mark.feature("conversation-attachments")
def test_linux_real_send_with_attachment_renders_in_sender_echo(
    real_faunamls_app, nest_instance, test_user,
):
    app = real_faunamls_app
    conv = app.conversations
    driver = app.driver

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Fixture setup (not the mutation under test): a real same-nest peer with a
    # real minted key package, then bootstrap a bound 1:1 channel with them via
    # the resolve-and-send RPC helper (proven by test_fauna_mls_real_roundtrip.py
    # on this same client). A bare handle (no @domain) classifies same-nest, so
    # no federation hop is needed to fetch the key package.
    bob = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    conv.real_resolve_send_new(bob["actor_id_hex"], "hi bob")

    deadline = time.time() + 20.0
    thread = None
    while time.time() < deadline and thread is None:
        for t in conv.list_threads():
            if t.rail == "FaunaMls" and "hi bob" in (t.snippet or ""):
                thread = t
                break
        if thread is None:
            time.sleep(1.0)
    assert thread, "the bootstrapped 1:1 with bob should surface its own echo"

    conv.open_thread_by_id(thread.thread_id)

    # The mutation under test: a real click on attachment-button + dm-send-button
    # on the now-bound thread -- no bootstrap needed this time, so send() goes
    # straight to encode_body -> blob_put -> post_app_message over the real wire.
    body = "here is the file"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    driver.click("dm-send-button")

    deadline = time.time() + 15.0
    while time.time() < deadline:
        if driver.count("dm-attachment-image") >= 1:
            break
        time.sleep(0.2)

    assert driver.count("dm-attachment-image") >= 1, (
        "a real send with a staged attachment must render dm-attachment-image "
        f"in the sender's own echo message: {driver.diagnose('dm-attachment-image')}"
    )
    texts = [driver.get_text("dm-message-text", index=i) for i in range(driver.count("dm-message-text"))]
    assert any(body in t for t in texts), f"sent body not found in any rendered message: {texts}"


@pytest.mark.tier_3
@pytest.mark.tui
@pytest.mark.feature("conversation-attachments")
def test_tui_real_send_with_attachment_renders_in_sender_echo(
    real_faunamls_app, nest_instance, test_user,
):
    """A real typed-path attach + a real send renders the attachment in tui's own
    echo — the whole production path (stage → seal → post → echo render) with no
    bypassed widget anywhere, since tui's ``attachment-button`` IS a path input.

    Waits are named budgets + deadline polls (convention 14), never fixed sleeps:
    a green run pays only the time the work actually takes, and the ceilings sit
    far above any non-pathological delay on a 20-session box.
    """
    app = real_faunamls_app
    conv = app.conversations
    driver = app.driver

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Fixture setup (not the mutation under test), exactly linux's: a real
    # same-nest peer with real minted key packages, then bootstrap a bound 1:1
    # channel. A bare handle (no @domain) classifies same-nest, so no federation
    # hop is needed to fetch the key package.
    bob = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    conv.real_resolve_send_new(bob["actor_id_hex"], "hi bob")

    def _bootstrapped_thread():
        for t in conv.list_threads():
            if t.rail == "FaunaMls" and "hi bob" in (t.snippet or ""):
                return t
        return None

    thread = wait_until(
        _bootstrapped_thread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"threads={[(t.rail, t.snippet) for t in conv.list_threads()]}",
    )
    assert thread, "the bootstrapped 1:1 with bob should surface its own echo"

    conv.open_thread_by_id(thread.thread_id)

    # The mutation under test: type the attachment's path into the real
    # `attachment-button` input and send. The thread is already bound, so send()
    # goes straight to encode_body → blob_put → post_app_message over the real
    # wire; nothing here is a helper that skips the control.
    body = "here is the file"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    assert not app.has_error(), f"staging the attachment failed: {app.error_text()}"
    assert driver.count("dm-compose-attachment-chip") == 1, (
        "the typed path must stage exactly one attachment before the send: "
        f"{driver.diagnose('dm-compose-attachment-chip')}"
    )

    driver.click("dm-send-button")

    wait_until(
        lambda: driver.count("dm-attachment-image") >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: driver.diagnose("dm-attachment-image"),
    )
    assert driver.count("dm-attachment-image") >= 1, (
        "a real send with a staged attachment must render dm-attachment-image "
        f"in the sender's own echo message: {driver.diagnose('dm-attachment-image')}"
    )
    texts = [
        driver.get_text("dm-message-text", index=i)
        for i in range(driver.count("dm-message-text"))
    ]
    assert any(body in t for t in texts), f"sent body not found in any rendered message: {texts}"


def _conversation_attachment_survives_a_blob_gc_sweep(app, nest_instance, test_user):
    """Shared body for every app's leg of "a file you sent is still there after
    the nest's storage clean-up runs" — a file sent in a conversation is still
    on the nest after the blob garbage collector runs with no grace period.

    The attachment is uploaded through ``POST /api/v1/blob`` sealed under the
    channel epoch key, and the only thing that named it was an MLS message the
    nest cannot open — so until 2026-09-09 the GC's reference walk had no
    conversation-derived source and every conversation attachment was swept on
    the first pass past the 30-minute grace, 404ing forever for every member
    (``docs/goal/behavior/backup-restore.md`` § 9 step 2, the *live
    conversation records* arm). The sender now lists the sealed blob's content
    address in plaintext beside the sealed envelope
    (``docs/goal/ui/conversations.md`` § Encryption at rest → *Attachment
    reachability*), and the nest pins it for as long as the record is live.

    **Why ONE body rather than one per app.** The hold is not app glue. The refs
    are filled by ``FaunaMlsBackend::encode_body`` from the very cids it just
    uploaded (``libs/fauna-conversations/src/backends/fauna_mls.rs`` — "one
    shared-Rust writer for all 7 apps", the goal section's own words) and pinned
    nest-side in ``conv_attachment_refs``; ``attachment_refs`` appears nowhere
    under ``apps/`` at all. So no column can hold the blob differently from
    another, and what is genuinely per-app is only how the test *reaches* the
    send: each app's ``attachment-button`` seam and its real-backend activation
    shape — both already arranged by the caller before this body runs. Copying
    the body per column would copy one claim seven times and invite the seven
    copies to drift apart.

    No e2e nest lives long enough to see a 6-hourly sweep, so this drives it
    directly: ``fauna.admin.gc`` with ``grace_period_secs: 0`` — "reclaim
    everything unreferenced right now" — against the real nest the real app
    just sent to. The witness is the sweep's own report: ``conv_attachment_refs``
    is the number of blobs the walk pinned through live conversation records'
    plaintext refs, and it rises by exactly one across the send only if the real
    app's real send carried the sealed blob's address and the nest recorded it
    beside the record — the whole floor path, app → wire → mirror row → oracle.
    A reading taken before the send is what makes that one attributable, and it
    is a baseline rather than a zero: the module shares one nest, so a sibling
    leg's own live attachment already pins refs. A pinned hash is never
    deleted (the pin set is the sweep's exclusion list), and that the listed hash
    IS the uploaded blob's key is pinned in shared Rust
    (``attachment_round_trips_outbound_to_inbound_faunamls``), so a live ref is
    the attachment's survival. A control orphan uploaded beside the attachment
    must be gone afterwards — the pass deleted, it did not merely count.

    **Staging is witnessed by the echo, not by the chip.**
    ``dm-compose-attachment-chip`` is outcome 2's own claim, witnessed per app by
    ``test_conversations_compose_attachments.py``; asserting it here would red
    outcome 5 on a column whose chip regressed while its attachment survived the
    sweep perfectly well — one outcome's cell moving for another outcome's
    reason. What this body needs is that the file really staged, and the
    sender's own echo (``dm-attachment-image``, appended only inside
    ``ConversationsManager::send``'s ``Ok`` arm) cannot render unless it did.
    ``app.has_error()`` is read first so a staging refusal diagnoses itself
    (e2e rule 11) rather than arriving a handshake later as a missing element,
    and the chip count rides in the wait's diagnose, where it informs without
    deciding.

    ⚠ Do NOT assert on ``deleted_blobs`` arithmetic here: a live app writes its
    MLS-state replica after every send (``__mls`` rail puts collapse their
    history, leaving superseded blobs a zero-grace pass legitimately reclaims),
    so "exactly N deleted" is unknowable under a real app — measured 2026-09-09:
    9 reclaimed against a 4 + 1 expectation with the attachment intact. The
    mechanism's mutant proof lives in the nest's ``gc.rs`` unit tests; this is
    the user-shaped witness through each app's real send.
    """
    import urllib.error
    import urllib.request

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.blob_upload import upload_blob

    conv = app.conversations
    driver = app.driver

    port = nest_instance["port"]
    admin = nest_instance["admin"]
    admin_sk = admin["signing_key"]

    def admin_call(kind, params):
        client = WsRpcAdminClient(
            nest_instance["url"],
            actor_id=bytes(admin_sk.verify_key),
            signing_key=bytes(admin_sk),
        )
        with client:
            return client.call(kind, params)

    # Fixture setup, exactly the sender-echo legs': a real same-nest peer with
    # real minted key packages, then a bound 1:1 so the send under test hits
    # encode_body → blob_put → channel.send with no bootstrap in the way.
    bob = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    conv.real_resolve_send_new(bob["actor_id_hex"], "hi bob")

    def _bootstrapped_thread():
        for t in conv.list_threads():
            if t.rail == "FaunaMls" and "hi bob" in (t.snippet or ""):
                return t
        return None

    thread = wait_until(
        _bootstrapped_thread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"threads={[(t.rail, t.snippet) for t in conv.list_threads()]}",
    )
    assert thread, "the bootstrapped 1:1 with bob should surface its own echo"
    conv.open_thread_by_id(thread.thread_id)

    # The arm's reading BEFORE the send, so the one below is attributable to
    # the send under test alone. It is a BASELINE, not a zero: `nest_instance`
    # is shared across the module, so a sibling leg's still-live attachment
    # record legitimately pins refs already (measured 2026-09-20: 2 on the web
    # leg, which runs after both sender-echo legs). Asserting an absolute zero
    # here would make this test's correctness depend on collection order —
    # green alone, red beside its own siblings — which is the same class of
    # brittleness convention 14 rules out for wall-clock waits. The delta is
    # exact either way: this send stages one attachment, so the sweep must pin
    # one more than it would have before it.
    baseline = admin_call("fauna.admin.gc", {"grace_period_secs": 0, "dry_run": True})
    assert baseline.get("dry_run") is True, f"the baseline must be a dry run, got {baseline!r}"
    refs_before = int(baseline.get("conv_attachment_refs", 0))

    # The mutation under test: the real attach through this app's own
    # attachment-button seam, then a real click on dm-send-button.
    body = "here is the file, and it stays"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    assert not app.has_error(), f"staging the attachment failed: {app.error_text()}"
    driver.click("dm-send-button")
    wait_until(
        lambda: driver.count("dm-attachment-image") >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"error-message={app.error_text()!r} "
            f"staged chips={driver.count('dm-compose-attachment-chip')} "
            f"{driver.diagnose('dm-attachment-image')}"
        ),
    )

    # The control orphan: a sealed-class-shaped upload nothing names. AEAD
    # shape is ≥28 bytes with no plaintext-content magic prefix; a byte ramp
    # starting at 0x20 is neither PNG, JPEG, GIF, PDF nor any other magic.
    orphan_hash = upload_blob(
        port, bob["token"], bytes(range(0x20, 0x60)), audience_class="Conversation"
    )
    assert len(orphan_hash) == 64, f"expected a 64-hex blob hash, got {orphan_hash!r}"

    reply = admin_call("fauna.admin.gc", {"grace_period_secs": 0, "dry_run": False})
    assert reply.get("dry_run") is False, f"the sweep must really run, got {reply!r}"
    assert int(reply.get("conv_attachment_refs", 0)) == refs_before + 1, (
        "the zero-grace sweep must have pinned exactly one more sealed attachment than "
        f"before the send — the one this send listed beside its envelope; had "
        f"{refs_before} before, got {reply!r}. No increase means nothing on the nest "
        "named the blob and it was swept."
    )
    assert int(reply["deleted_blobs"]) >= 1, f"the control orphan must have been swept: {reply!r}"

    # The control really went (the pass deleted, it did not just count) …
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/blob/{orphan_hash}", method="GET"
    )
    try:
        with urllib.request.urlopen(req, timeout=10):
            raise AssertionError("the control orphan survived a zero-grace sweep")
    except urllib.error.HTTPError as e:
        assert e.code == 404, f"expected the control orphan to be gone (404), got {e.code}"

    # … and the sent attachment still renders in the thread.
    assert driver.count("dm-attachment-image") >= 1, (
        "the attachment should still render after the sweep: "
        f"{driver.diagnose('dm-attachment-image')} error={app.error_text()!r}"
    )


@pytest.mark.tier_3
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("conversation-attachments")
def test_real_faunamls_conversation_attachment_survives_a_blob_gc_sweep(
    real_faunamls_app, nest_instance, test_user,
):
    """The runtime-toggle trio's leg — web, linux and tui flip the real
    ``FaunaMlsBackend`` live on the session-cached app, so no
    ``real_conversations`` marker is involved and the three share one collected
    test, exactly as the oversized-attachment sibling above does.

    Each still reaches ``attachment-button`` its own way — web through a real
    ``<input type="file">`` Playwright sets over CDP, linux through the
    compose-state-protocol stand-in for its undrivable GTK portal, tui through
    the production typed-path input itself (all three rationales in the module
    docstring) — and all three then run the same shared body, because from the
    staged file onward the path is shared Rust and the nest.
    """
    _conversation_attachment_survives_a_blob_gc_sweep(
        real_faunamls_app, nest_instance, test_user
    )


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.windows
@pytest.mark.feature("conversation-attachments")
def test_windows_conversation_attachment_survives_a_blob_gc_sweep(
    real_faunamls_app, nest_instance, test_user,
):
    """Windows' leg. Windows is a LAUNCH-TIME real-backend gate rather than the
    trio's runtime toggle, so it needs ``real_conversations`` and therefore its
    own collected test (``real_faunamls_app``'s docstring; a windows test
    consuming that fixture without the marker raises a legible readiness-poll
    timeout, never a silent mock pass). Staging is the same
    compose-state-protocol seam its sender-echo leg below already proves.
    """
    _conversation_attachment_survives_a_blob_gc_sweep(
        real_faunamls_app, nest_instance, test_user
    )


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.android
@pytest.mark.feature("conversation-attachments")
def test_android_conversation_attachment_survives_a_blob_gc_sweep(
    real_faunamls_app, nest_instance, test_user,
):
    """Android's leg — windows' launch-gate shape, staged through
    ``TestAgent.kt``'s ``compose.file[attachment-button]`` arm (its sender-echo
    leg below says how). Nothing android-specific sits behind the hold: the
    refs are written by shared Rust and pinned by the nest (the body's
    docstring).
    """
    _conversation_attachment_survives_a_blob_gc_sweep(
        real_faunamls_app, nest_instance, test_user
    )


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.macos
@pytest.mark.feature("conversation-attachments")
def test_macos_conversation_attachment_survives_a_blob_gc_sweep(
    real_faunamls_app, nest_instance, test_user,
):
    """macOS' leg — the launch gate again, plus the anti-mock guard apple needs
    because it has no client-side "is the real backend up" signal at all.
    """
    _assert_apple_real_backend_is_live(nest_instance["port"], test_user)
    _conversation_attachment_survives_a_blob_gc_sweep(
        real_faunamls_app, nest_instance, test_user
    )


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.ios
@pytest.mark.feature("conversation-attachments")
def test_ios_conversation_attachment_survives_a_blob_gc_sweep(
    real_faunamls_app, nest_instance, test_user,
):
    """iOS' leg — macOS' twin; see it and ``_assert_apple_real_backend_is_live``."""
    _assert_apple_real_backend_is_live(nest_instance["port"], test_user)
    _conversation_attachment_survives_a_blob_gc_sweep(
        real_faunamls_app, nest_instance, test_user
    )


def _launch_gated_real_send_with_attachment(app, nest_instance, test_user):
    """Shared sender-echo body for the launch-gate columns that run on ordinary
    budgets — windows and android. Structurally linux's leg: both reach
    ``real_faunamls_app`` through the LAUNCH-TIME ``real_conversations`` gate
    rather than linux/tui's runtime toggle (so each leg carries the marker and
    its module runs in its own invocation), and both stage through the
    compose-state-protocol ``set_input_files`` seam — windows' ``App.xaml.cs``
    routes ``target: "attachment-button"`` to
    ``ConversationsPage.Current.StageAttachment``, android's ``TestAgent.kt`` to
    the shared ``addAttachment``/``addNewThreadAttachment`` its ``GetContent()``
    picker callbacks call. From the staged file onward the path is shared Rust
    and the nest, so one body serves both; the apple pair keeps its own for the
    slower real-MLS bootstrap budget (``_apple_real_send_with_attachment``).
    """
    conv = app.conversations
    driver = app.driver

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Fixture setup (not the mutation under test): a real same-nest peer with a
    # real minted key package, then bootstrap a bound 1:1 channel with them via
    # the resolve-and-send RPC helper (proven by test_fauna_mls_real_roundtrip.py
    # on this same client). A bare handle (no @domain) classifies same-nest, so
    # no federation hop is needed to fetch the key package.
    bob = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    conv.real_resolve_send_new(bob["actor_id_hex"], "hi bob")

    def _bootstrapped_thread():
        for t in conv.list_threads():
            if t.rail == "FaunaMls" and "hi bob" in (t.snippet or ""):
                return t
        return None

    thread = wait_until(
        _bootstrapped_thread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"threads={[(t.rail, t.snippet) for t in conv.list_threads()]}",
    )
    assert thread, "the bootstrapped 1:1 with bob should surface its own echo"

    conv.open_thread_by_id(thread.thread_id)

    # The mutation under test: a real click on attachment-button (staged via
    # the same compose-state-protocol bypass the inbound module proves) +
    # dm-send-button on the now-bound thread -- send() goes straight to
    # encode_body -> blob_put -> post_app_message over the real wire.
    body = "here is the file"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    assert not app.has_error(), f"staging the attachment failed: {app.error_text()}"

    driver.click("dm-send-button")

    wait_until(
        lambda: driver.count("dm-attachment-image") >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: driver.diagnose("dm-attachment-image"),
    )
    assert driver.count("dm-attachment-image") >= 1, (
        "a real send with a staged attachment must render dm-attachment-image "
        f"in the sender's own echo message: {driver.diagnose('dm-attachment-image')}"
    )
    texts = [driver.get_text("dm-message-text", index=i) for i in range(driver.count("dm-message-text"))]
    assert any(body in t for t in texts), f"sent body not found in any rendered message: {texts}"


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.windows
@pytest.mark.feature("conversation-attachments")
def test_windows_real_send_with_attachment_renders_in_sender_echo(
    real_faunamls_app, nest_instance, test_user,
):
    """Windows' leg: windows already wires `attachment-button`'s click handler
    (module docstring — windows was the one client that had it before
    2026-07-20) and stages via the same compose-state-protocol
    `set_input_files` bypass `test_conversations_compose_attachments.py`
    already proves for windows. A windows test consuming `real_faunamls_app`
    without the `real_conversations` marker raises a legible readiness-poll
    timeout, never a silent mock pass (the fixture's docstring).
    """
    _launch_gated_real_send_with_attachment(real_faunamls_app, nest_instance, test_user)


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.android
@pytest.mark.feature("conversation-attachments")
def test_android_real_send_with_attachment_renders_in_sender_echo(
    real_faunamls_app, nest_instance, test_user,
):
    """Android's leg — windows' shape. ``attachment-button`` opens a real
    ``GetContent()`` picker, an OS activity outside the app's own Compose tree,
    so ``set_input_files`` goes through ``TestAgent.kt``'s
    ``compose.file[attachment-button]`` arm, which resolves the open composer
    off the shared manager's snapshot (an active new-thread compose, else the
    thread ``conversation-item``'s tap selected) exactly as linux's agent and
    apple's ``ConversationsVM.attachComposerFile`` do, EXIF-strips like the real
    callback, and calls the same ``addAttachment``. Only the OS picker is
    bypassed. The real-rail control is the fixture's (``real_faunamls_app``).
    """
    _launch_gated_real_send_with_attachment(real_faunamls_app, nest_instance, test_user)


def _apple_real_send_with_attachment(app, nest_instance, test_user):
    """Shared body for the macOS + iOS legs -- structurally linux's leg with
    apple-sized timeouts (apple's real-MLS bootstrap is materially slower than
    linux's, and a macOS VM routinely carries several live sessions).

    One leg per client rather than a client-parametrized single test, matching
    this module's existing per-app shape: each app's bypass rationale
    differs and the markers drive ``--client`` deselection.
    """
    conv = app.conversations
    driver = app.driver

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    _assert_apple_real_backend_is_live(port, test_user)

    # Fixture setup (not the mutation under test), identical to linux's leg: a
    # real same-nest peer with a real minted key package, then bootstrap a bound
    # 1:1 so the send under test never has to bootstrap.
    bob = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    conv.real_resolve_send_new(bob["actor_id_hex"], "hi bob")

    deadline = time.time() + 90.0
    thread = None
    while time.time() < deadline and thread is None:
        for t in conv.list_threads():
            if t.rail == "FaunaMls" and "hi bob" in (t.snippet or ""):
                thread = t
                break
        if thread is None:
            time.sleep(1.0)
    assert thread, (
        "the bootstrapped 1:1 with bob should surface its own echo; have "
        f"{[(t.rail, t.snippet) for t in conv.list_threads()]}"
    )

    conv.open_thread_by_id(thread.thread_id)

    # The mutation under test: staging through the real attachment seam
    # (ConversationsVM.attachFile -> manager.add_attachment) then a real click
    # on dm-send-button, on the now-bound thread.
    body = "here is the file"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    # The agent reports a staging failure on error-message rather than dropping
    # the command (e2e rule 11) -- surface it now, before the send masks it as a
    # missing-attachment assertion 45s later.
    assert not app.has_error(), f"staging the attachment failed: {app.error_text()}"

    driver.click("dm-send-button")

    deadline = time.time() + 45.0
    while time.time() < deadline:
        if driver.count("dm-attachment-image") >= 1:
            break
        time.sleep(0.5)

    assert driver.count("dm-attachment-image") >= 1, (
        "a real send with a staged attachment must render dm-attachment-image "
        f"in the sender's own echo message (error-message: {app.error_text()!r}): "
        f"{driver.diagnose('dm-attachment-image')}"
    )
    texts = [driver.get_text("dm-message-text", index=i) for i in range(driver.count("dm-message-text"))]
    assert any(body in t for t in texts), f"sent body not found in any rendered message: {texts}"


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.macos
@pytest.mark.feature("conversation-attachments")
def test_macos_real_send_with_attachment_renders_in_sender_echo(
    real_faunamls_app, nest_instance, test_user,
):
    _apple_real_send_with_attachment(real_faunamls_app, nest_instance, test_user)


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.ios
@pytest.mark.feature("conversation-attachments")
def test_ios_real_send_with_attachment_renders_in_sender_echo(
    real_faunamls_app, nest_instance, test_user,
):
    _apple_real_send_with_attachment(real_faunamls_app, nest_instance, test_user)


def _apple_real_send_with_oversized_attachment(app, nest_instance, test_user, tmp_path):
    """Shared body for the macOS + iOS oversized legs -- structurally the
    web/linux/tui leg
    (``test_real_faunamls_send_with_oversized_attachment_surfaces_on_error_message``)
    with apple-sized timeouts, the same way
    :func:`_apple_real_send_with_attachment` mirrors its own sibling.

    The rejection is genuinely the nest's: 15 MiB exceeds the global 10 MiB
    ``RequestBodyLimitLayer`` on ``/api/v1/blob``
    (``bins/fauna-nest/src/lib.rs``), so ``blob_put`` fails on a real multipart
    POST over apple's native ``fauna_nest_http`` transport -- no
    ``inject_send_failure_for_test`` seam, which is the whole point of the
    outcome (``docs/features/conversation-attachments.md`` outcome 4).

    Reads ``error-message`` through the driver directly rather than
    ``app.error_text()``, for the reason that test's docstring records: the
    action layer's state-protocol shortcut returns ``""`` without ever reading
    the element, so it cannot settle whether the element rendered.
    """
    conv = app.conversations
    driver = app.driver

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    _assert_apple_real_backend_is_live(port, test_user)

    # Fixture setup (not the mutation under test -- e2e rule 8 carve-out (b)),
    # identical to the sender-echo leg above.
    bob = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    conv.real_resolve_send_new(bob["actor_id_hex"], "hi bob")

    thread = wait_until(
        lambda: next(
            (
                t
                for t in conv.list_threads()
                if t.rail == "FaunaMls" and "hi bob" in (t.snippet or "")
            ),
            None,
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"threads={[(t.rail, t.snippet) for t in conv.list_threads()]}",
    )
    conv.open_thread_by_id(thread.thread_id)

    assert not driver.is_visible("error-message"), (
        "a freshly opened bootstrapped thread must show no error"
    )

    # The mutation under test: a real UI attach + send of a file the nest's real
    # blob route genuinely refuses.
    oversized = tmp_path / "oversized-attachment.bin"
    oversized.write_bytes(b"\0" * (15 * 1024 * 1024))

    body = "here is a file too big to accept"
    driver.clear_and_type("dm-text-field", body)
    driver.set_input_files("attachment-button", str(oversized))
    driver.click("dm-send-button")

    wait_until(
        lambda: driver.is_visible("error-message"),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"error_text={app.error_text()!r} "
            f"dm-attachment-image count={driver.count('dm-attachment-image')} "
            f"{driver.diagnose('error-message')}"
        ),
    )

    shown = driver.get_text("error-message")
    assert "conversations.unified" not in shown, (
        f"the i18n key must be resolved through the app's pipeline, not painted raw: {shown!r}"
    )
    assert "send" in shown.lower(), (
        f"the resolved text must be the send-failure template ({shown!r})"
    )
    # A real rejection must never let the sender's own echo render either --
    # `ConversationsManager::send` only appends it inside the `Ok` arm.
    assert driver.count("dm-attachment-image") == 0, (
        "a genuinely rejected upload must not render a sender echo "
        f"(got {driver.count('dm-attachment-image')})"
    )


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.macos
@pytest.mark.feature("conversation-attachments")
def test_macos_real_send_with_oversized_attachment_surfaces_on_error_message(
    real_faunamls_app, nest_instance, test_user, tmp_path,
):
    _apple_real_send_with_oversized_attachment(
        real_faunamls_app, nest_instance, test_user, tmp_path
    )


@pytest.mark.tier_3
@pytest.mark.real_conversations
@pytest.mark.ios
@pytest.mark.feature("conversation-attachments")
def test_ios_real_send_with_oversized_attachment_surfaces_on_error_message(
    real_faunamls_app, nest_instance, test_user, tmp_path,
):
    _apple_real_send_with_oversized_attachment(
        real_faunamls_app, nest_instance, test_user, tmp_path
    )
