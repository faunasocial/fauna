"""tier_3 — the per-set WebDAV serve toggle (`folder-webdav-toggle`), client-UI
driven (webdav-server.md § Independent enablement point 2 / folders.md § Element
IDs).

The per-set toggle drives `FoldersAuthor::serve_set` — serve_enable/serve_disable
(content-key genesis/rotation + the nest `folders.webdav_enabled` flag) + the
MSEK-sealed `WebdavKeysBlob` re-provision — through the conversations-rail wasm
face (`foldersServeSet`), the production caller of the slice-2 serve orchestration
that `test_webdav_read_write_roundtrip.py` proves end-to-end. This test covers the
web APP UI path: flipping a row's toggle serves and unserves the set.

The MUTATION is UI-driven (the toggle
click); VERIFICATION reads the nest ground truth via `fauna.folders.list`
(external, mirroring test_admin_files' get_mail_config read). serve_set's blob
re-provision seals under the actor's MSEK, which every account holds from its
first connect.

The no-mail test below (the only one needing a mail-*less* actor) runs against
`dedicated_no_mail_app`/`dedicated_no_mail_nest` — a freshly-registered actor on
its own nest, never touched by the shared session's `test_user`, whose
SESSION-scoped actor other files give a mail credential long before this module
runs. It asserts the precondition rather than assuming it. The other two tests
use the shared `logged_in_app`/`nest_instance`/`test_user` and call
`ensure_mail_enabled()` themselves, so they never depended on ordering.
"""

import secrets
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.set_names import find_set

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # The serve rides a live `ConversationsSession` (`folders_serve_set` takes one),
    # which the native apps build only under the real receive loop — linux/web run it
    # for every e2e login by construction, macOS/iOS only under this flag
    # (`FaunaE2E.realConversations` → `applySessionPatch` builds + activates the real
    # session). Without it `awaitSharedConversationsSession()` has nothing to await and
    # throws, so the serve tests below cannot pass. Session-wide across this module by
    # design (`_apply_real_conversations_env`); the module carries no mock-inject DM
    # test, so nothing here regresses under it — validated on macos + ios.
    pytest.mark.real_conversations,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
    # tui lifted the toggle 2026-07-30 (`settings/folders.rs`, the expanded
    # owner row body) — the last app to get it. Like linux/web it builds the
    # real `ConversationsSession` on every e2e login by construction, so it
    # ignores the `real_conversations` flag above.
    pytest.mark.tui,
]


def _user_client(nest_instance, test_user):
    """A User-class WS-RPC client on the logged-in actor — the ground-truth read
    for the per-set `webdav_enabled` flag. `WsRpcAdminClient` is identity-generic
    (its docstring: "pass a User-class actor's keypair and you have a User-class
    client"); `.call` works for any kind."""
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _served(client, name: str) -> bool:
    """Nest-authoritative per-set serve flag from `fauna.folders.list`, the row
    found by its name hash — a sealed set rests no plaintext name, so a
    `fs["name"] == name` match finds nothing (`helpers/set_names`)."""
    reply = client.call("fauna.folders.list", {})
    row = find_set(reply.get("folders", []), name)
    return bool(row and row.get("webdav_enabled"))


def _wait(pred, timeout: float = 20.0, interval: float = 0.3) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(interval)
    return pred()


@pytest.mark.feature("share-a-folder")
def test_folder_webdav_toggle_serves_on_an_account_with_no_mail(
    dedicated_no_mail_app, dedicated_no_mail_nest
):
    """An account that never set up mail can serve a folder: the switch waits on
    the account's mail key alone, and every account holds that key from its
    first connect (`folders.md` § Errors & edge cases, *Own-tier and mail
    gates*, ruled 2026-11-19; `mail-credentials.md` § Every account holds the
    key). No mailbox and no credential is asked for — the credential an outside
    file app signs in with comes after the flip, from the mail page section the
    served folder opens.

    Until 2026-11-19 this slot held the opposite journey ("disabled until mail
    is set up"): the key was then minted by the first mail enable, so a no-mail
    account could not serve. The connect-time mint made that state unreachable.

    Runs against `dedicated_no_mail_app` (its own nest + a freshly-registered
    actor, see module docstring), so "this account has no mail" is true by
    construction; the precondition is still asserted, and asserted again after
    the flip — serving must not mint a credential on the way.
    """
    app = dedicated_no_mail_app
    b = app.backups
    gt = _user_client(dedicated_no_mail_nest, dedicated_no_mail_nest["user"])

    app.mail_settings.navigate()
    assert app.mail_settings.credential_count() == 0, (
        "dedicated_no_mail_app's freshly-registered actor already holds a mail "
        "credential — the journey would prove nothing about a no-mail account"
    )

    name = f"webdav-nomail-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)

    def _toggle_enabled_reexpanding() -> bool:
        # linux rebuilds the folder list on every DevicesMachine observer tick,
        # collapsing the expander (webdav_toggle_visible's docstring) — re-expand
        # on every check. The switch may read "still loading" first: the custody
        # read, or the connect-time mint, can still be on its way.
        if not b.webdav_toggle_visible():
            b.find_and_expand_folder(name)
        return b.webdav_toggle_enabled()

    assert _wait(_toggle_enabled_reexpanding, timeout=30.0), (
        "folder-webdav-toggle must be ENABLED on an account with no mail once "
        "its mail key is read (minted at first connect); "
        f"error={app.error_text()!r}"
    )

    with gt:
        assert not _served(gt, name), "a freshly-created set is not served over WebDAV"
        b.toggle_webdav()
        assert _wait(lambda: _served(gt, name)), (
            f"toggling folder-webdav-toggle ON must serve {name!r} on an account "
            f"with no mail; error={app.error_text()!r}"
        )
    assert not app.has_error(), f"serve-on raised an error: {app.error_text()!r}"

    app.mail_settings.navigate()
    assert app.mail_settings.credential_count() == 0, (
        "serving a folder must not mint a mail credential"
    )


@pytest.mark.feature("share-a-folder")
def test_folder_webdav_toggle_serves_and_unserves_a_sync_set(
    logged_in_app, nest_instance, test_user
):
    # (windows was xfail here until 2026-07-14: its e2e `set_state` login never
    # threaded a `ConversationsSession` into `ServiceClients.ConvSession`, so every
    # `_convSession`-gated folder gesture silently no-opped under the harness.
    # `App.BuildE2eConvSessionAsync` now builds the real session on every e2e login,
    # exactly as linux's `conv_backend::start_conversations_session` does.)
    app = logged_in_app
    b = app.backups
    gt = _user_client(nest_instance, test_user)

    # Precondition: serve_set's WebdavKeysBlob re-provision seals under the
    # actor's MSEK, minted when mail is first enabled.
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=app.mail_settings.ENABLE_SETTLE_S), (
        "mail must enable to mint the MSEK serve_set seals under; "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    name = f"webdav-set-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    # The ground-truth reads run over one open WS connection (WsRpcAdminClient
    # opens on `with`, mirroring test_admin_files); the web app's own serve
    # calls ride separate connections, so this never contends.
    with gt:
        assert not _served(gt, name), "a freshly-created sync set is not served over WebDAV"

        # MUTATION (UI): flip the per-set toggle ON → serve_set(enable=true).
        b.find_and_expand_folder(name)
        b.toggle_webdav()

        # VERIFICATION (nest ground truth): the per-set flag flipped on, no UI error.
        assert _wait(lambda: _served(gt, name)), (
            f"toggling folder-webdav-toggle ON must serve {name!r}; error={app.error_text()!r}"
        )
        assert not app.has_error(), f"serve-on raised an error: {app.error_text()!r}"

        # MUTATION (UI): flip OFF → serve_disable(enable=false) + blob re-provision
        # without the set. The toggle re-rendered to checked after the refresh; on a
        # client that rebuilds the whole list on every observer tick (e.g. linux) the
        # refresh also re-collapses the expander, so re-expand before the second click
        # if needed (a no-op re-expand on a client like web, where it stays open).
        if not b.webdav_toggle_visible():
            b.find_and_expand_folder_until(name, "folder-webdav-toggle")
        b.toggle_webdav()
        assert _wait(lambda: not _served(gt, name)), (
            f"toggling folder-webdav-toggle OFF must unserve {name!r}; error={app.error_text()!r}"
        )
        assert not app.has_error(), f"serve-off raised an error: {app.error_text()!r}"


@pytest.mark.feature("files-in-standard-apps")
def test_mua_webdav_url_row_tracks_the_per_actor_serve_state(
    logged_in_app, nest_instance, test_user
):
    """Slice 6b-2: the MUA-setup panel's WebDAV URL row (`mail-settings-mua-webdav-url`).

    `webdav-server.md` § Independent enablement pt 1 — "the MUA-setup panel gains
    WebDAV ... lines (render when enabled)"; `mail-settings.md` § WebDAV files
    pins the concrete shape: ONE full URL row (`https://mail.<domain>/webdav/`),
    not a host+port pair, because no SRV autodiscovery exists for WebDAV, so this
    line is the primary setup surface.

    The row is gated on the **per-actor** serve state (`MailSettingsSnapshot.
    serves_webdav_set`, folded from `fauna.folders.list`), NOT the deployment-wide
    `webdav_enabled` toggle — that flag defaults ON for a real-domain box, so gating
    on it would show a dead mount URL to every actor serving nothing.

    MUTATION is UI-driven throughout (the per-set toggle click); the assertion reads
    the rendered row.
    """
    # (windows was xfail here until 2026-07-14 — its precondition is the serve
    # toggle above, which the e2e login's missing ConversationsSession blocked.)
    app = logged_in_app
    b = app.backups

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    assert app.mail_settings.mua_instructions_visible(), (
        "MUA-instructions block should render once mail is enabled; "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    # An actor serving no set has no WebDAV row — the collection root would be empty.
    assert not app.mail_settings.mua_webdav_url_visible(timeout=3.0), (
        "the WebDAV URL row must stay hidden while the actor serves no folder"
    )

    name = f"webdav-mua-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    # MUTATION (UI): serve the set, which is what makes the URL mountable.
    b.find_and_expand_folder(name)
    b.toggle_webdav()
    assert not app.has_error(), f"serve-on raised an error: {app.error_text()!r}"

    # The row appears on the next mail-settings hydrate (the snapshot re-folds
    # `fauna.folders.list`).
    app.mail_settings.navigate()
    assert app.mail_settings.mua_webdav_url_visible(timeout=15.0), (
        "serving a folder must surface the WebDAV URL row; "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    url = app.mail_settings.mua_webdav_url()
    # A populated, placeholder-free, mountable collection root. The tier_3 nest is
    # reached at 127.0.0.1 — a local target — so the URL is the bare locator plus
    # the DAV port rather than `mail.<domain>` (the same any-locator carve-out the
    # IMAP/CalDAV host rows take; `caldav-server.md` § Network exposure).
    assert url and "{" not in url, f"WebDAV URL should be placeholder-free; got {url!r}"
    assert url.startswith("https://"), f"WebDAV URL should be https; got {url!r}"
    assert url.endswith("/webdav/"), (
        f"WebDAV URL should be the collection root ending in /webdav/; got {url!r}"
    )

    # MUTATION (UI): unserve → the row retreats with the per-actor serve state.
    b.navigate_folders()
    if not b.webdav_toggle_visible():
        b.find_and_expand_folder_until(name, "folder-webdav-toggle")
    b.toggle_webdav()
    assert not app.has_error(), f"serve-off raised an error: {app.error_text()!r}"
    app.mail_settings.navigate()
    assert app.mail_settings.mua_webdav_url_hidden(), (
        "unserving the last folder must hide the WebDAV URL row again"
    )
