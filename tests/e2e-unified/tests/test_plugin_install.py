"""tier_3: an admin installs a hosted plugin from the app, end to end
(``docs/goal/architecture/third-party.md`` § The runner contract → *The
install-approval leg*; ``docs/goal/behavior/bridges.md`` § Hosted plugins on
this page; ``docs/goal/ui/connected-apps.md``).

The admin gives the plugin's document URL on the Bridges page; the nest
resolves the document, verifies the publisher's signed manifest against the
document's host, fetches the module and checks its pinned digest, and opens
the install card — the same consent card every request uses, with an install
section — in Connected apps → Requests. The section names what would run on
the nest: the execution form, the publisher's domain and ``did:key``, the
module digest, the kinds, the network reach and the settings summary.
Approving starts the plugin, which the Bridges page then lists as running;
Uninstall (behind an inline confirm) removes it. The second test is the bind
leg (§ The principal model → *Hosted principals*): the account Connects from
the plugin's settings card, approves the bind card in the same tray, and
Disconnects through the roster's one verb.

The plugin is the in-repo hello plugin (``hello_plugin.wat``), componentized
by the ``plugin-fixture-gen`` helper and served — document and module — by
``helpers/client_metadata_server.py`` on a real hostname, which the nest's
``test-hooks`` resolve map points at a loopback port. Every mutation goes
through the app (convention 8); the nest's own list is read alongside to say
which side failed (convention 5).

**Install-and-uninstall test:** tui (the lead), linux, web and android; the
other three apps follow in the batched trickle-down (third-party.md §
Implementation status today). The bind test below stays tui only.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519

from clients.ws_rpc_admin_client import WsRpcAdminClient
from i18n.strings import S
from helpers.client_metadata_server import (
    ClientMetadataServer,
    ed25519_did_key,
    sign_manifest,
)

pytestmark = [pytest.mark.tier_3]

_HOST = "plugins.example"
_DOCUMENT_PATH = "/hello.json"
_MODULE_PATH = "/hello.wasm"
_KIND = f"ext.{_HOST}.notes"
_NAME = "Hello Plugin"


def _hello_component(out: Path) -> bytes:
    """Build (cargo no-ops when fresh) and run the ``plugin-fixture-gen``
    e2e helper: the hello plugin's component, written to ``out``."""
    from common.nest import _build_slot_cmd, _cargo_cmd, get_repo_root

    repo_root = get_repo_root()
    subprocess.run(
        _build_slot_cmd(repo_root)
        + _cargo_cmd(repo_root)
        + [
            "build",
            "-p",
            "fauna-plugin-host",
            "--features",
            "test-helpers",
            "--bin",
            "plugin-fixture-gen",
        ],
        cwd=repo_root,
        check=True,
    )
    exe = ".exe" if sys.platform == "win32" else ""
    name = f"plugin-fixture-gen{exe}"
    candidates = []
    if os.environ.get("CARGO_TARGET_DIR"):
        candidates.append(Path(os.environ["CARGO_TARGET_DIR"]) / "debug" / name)
    candidates.append(repo_root / "target" / "debug" / name)
    binary = next((p for p in candidates if p.exists()), None)
    assert binary is not None, f"{name} not found after build (looked in {candidates})"
    subprocess.run([str(binary), str(out)], check=True)
    return out.read_bytes()


@pytest.fixture(scope="module")
def plugin_publisher(tmp_path_factory):
    """The publisher's document + module on ``plugins.example``: a signed
    manifest whose ``execution`` pins the hello plugin's digest, one kind,
    one setting, no outbound hosts."""
    tmp = tmp_path_factory.mktemp("plugin-publisher")
    module = _hello_component(tmp / "hello.wasm")
    digest = f"sha256:{hashlib.sha256(module).hexdigest()}"
    key = ed25519.Ed25519PrivateKey.generate()
    public = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    server = ClientMetadataServer(_HOST, tmp)
    payload = {
        "version": 1,
        "publisher": {"domain": _HOST, "key": ed25519_did_key(public)},
        "execution": {
            "form": "wasm",
            "module": f"https://{_HOST}{_MODULE_PATH}",
            "digest": digest,
            "hosts": [],
        },
        "kinds": [{"kind": _KIND, "class": "state", "merge": "latest-wins", "floor": "none"}],
        "settings_schema": {
            "greeting": {"label": "Greeting", "type": "string", "value": "hi"},
        },
        "ingress": [],
    }
    server.documents[_DOCUMENT_PATH] = {
        "client_id": server.client_id(_DOCUMENT_PATH),
        "client_name": _NAME,
        "redirect_uris": [f"https://{_HOST}/cb"],
        "scope": f"atproto fauna:records:rw:ext.{_HOST}.*",
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "application_type": "web",
        "dpop_bound_access_tokens": True,
        "fauna": sign_manifest(key, payload),
    }
    server.blobs[_MODULE_PATH] = module
    yield {
        "server": server,
        "document_url": server.client_id(_DOCUMENT_PATH),
        "digest": digest,
        "did_key": ed25519_did_key(public),
    }
    server.close()


@pytest.fixture
def plugin_nest(nest_binary, tmp_path_factory, plugin_publisher):
    """A nest whose guarded fetcher reaches the publisher's loopback server."""
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary,
        tmp_path_factory,
        "plugin-install",
        extra_env=plugin_publisher["server"].nest_env(),
    )
    yield nest
    cleanup()


@pytest.fixture
def plugin_spa_url(spa_proxy_for, plugin_nest):
    """The SPA proxy a web seat would dial this nest through."""
    return spa_proxy_for(plugin_nest["url"])


@pytest.fixture
def plugin_admin_app(request, app, plugin_nest):
    from conftest import _login_admin_as

    _login_admin_as(
        app,
        request,
        plugin_nest,
        spa_url_fixture="plugin_spa_url",
        fixture_name="plugin_admin_app",
    )
    return app


def _as_owner(nest, kind: str) -> dict:
    """One call as the nest's admin — the account the app is signed in as."""
    sk = nest["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)
    ) as ws:
        return ws.call(kind, {})


def _listed(nest) -> list[dict]:
    """The nest's own ``fauna.plugins.list``, as its admin."""
    return _as_owner(nest, "fauna.plugins.list")["plugins"]


def _grant_ids(nest) -> set[bytes]:
    """The owner's live capability grants, by id (``fauna.capabilities.reconcile``)."""
    return {bytes(g) for g in _as_owner(nest, "fauna.capabilities.reconcile")["grant_ids"]}


def _install_from_the_app(app, publisher) -> None:
    """The admin installs the plugin from the Bridges page and approves its
    install card in Connected apps — the first test's journey, which the
    second starts from."""
    bridges, ca = app.bridges, app.connected_apps
    bridges.navigate()
    assert bridges.is_page_visible(), "the Bridges page did not render"
    bridges.install_plugin(publisher["document_url"])
    error = bridges.page_error_text(timeout=2.0)
    assert not error, f"the install was refused: {error!r}"
    ca.navigate()
    i = ca.wait_for_request_from(publisher["document_url"])
    assert i is not None, (
        "the install card never reached Connected apps → Requests. "
        f"error: {ca.current_error_text()!r}"
    )
    ca.approve_request(i)


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.standalone_only
@pytest.mark.feature("bridges")
def test_an_admin_installs_a_plugin_from_its_card_and_uninstalls_it(
    plugin_admin_app, plugin_nest, plugin_publisher
):
    app = plugin_admin_app
    bridges = app.bridges
    ca = app.connected_apps

    # ── Install: the document URL on the Bridges page opens the card ──
    bridges.navigate()
    assert bridges.is_page_visible(), "the Bridges page did not render"
    bridges.install_plugin(plugin_publisher["document_url"])
    error = bridges.page_error_text(timeout=2.0)
    assert not error, f"the install was refused: {error!r}"

    # ── The card lands in the tray with its install section ──
    ca.navigate()
    i = ca.wait_for_request_from(plugin_publisher["document_url"])
    assert i is not None, (
        "the install card never reached Connected apps → Requests. "
        f"error: {ca.current_error_text()!r}"
    )
    assert ca.request_install_text(i), "the card carries no install section"
    assert ca.request_install_form(i) == "wasm"
    publisher = ca.request_install_text(i, "publisher")
    assert _HOST in publisher and plugin_publisher["did_key"] in publisher, publisher
    assert plugin_publisher["digest"] in ca.request_install_text(i, "digest")
    assert _KIND in ca.request_install_text(i, "kinds")
    hosts = ca.request_install_text(i, "hosts")
    assert hosts and _HOST not in hosts, f"a plugin declaring no host reaches none: {hosts!r}"
    assert "Greeting (string)" in ca.request_install_text(i, "settings")

    # ── Approve: the plugin runs, and the Bridges page lists it ──
    ca.approve_request(i)
    bridges.navigate()
    row = bridges.wait_for_plugin(_NAME, state="running")
    listed = _listed(plugin_nest)
    assert row is not None, (
        f"the Bridges page never listed {_NAME!r} running. "
        f"page error: {bridges.page_error_text(timeout=0)!r}; "
        f"the nest lists: {json.dumps(listed, default=str)}"
    )
    assert [p["client_id"] for p in listed] == [plugin_publisher["document_url"]]
    assert plugin_publisher["did_key"] in bridges.plugin_publisher(row)

    # ── Uninstall (arm, then confirm) removes it ──
    bridges.uninstall_plugin(row)
    assert bridges.wait_for_no_plugin(_NAME), (
        f"the plugin was still listed after Uninstall. "
        f"page error: {bridges.page_error_text(timeout=0)!r}"
    )
    assert _listed(plugin_nest) == [], "the nest still lists the plugin"


def _available(nest) -> list[dict]:
    """The nest's own ``fauna.plugins.available`` for the signed-in account."""
    return _as_owner(nest, "fauna.plugins.available")["plugins"]


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.standalone_only
@pytest.mark.feature("bridges")
def test_a_user_connects_a_plugin_from_its_settings_card_and_disconnects_it(
    plugin_admin_app, plugin_nest, plugin_publisher
):
    """``third-party.md`` § The principal model → *Hosted principals*, the bind
    leg: an install grants nothing, so the account binds the plugin by its own
    consent from the plugin's settings card on the Bridges page. *Connect*
    opens the bind card into Connected apps → Requests; approving it there —
    the one approve path, which mints the account's grant to the plugin's
    holder key before it resolves — makes the plugin a connected roster row
    with its ``wasm`` badge and a link back to the card. *Disconnect* is the
    roster's one verb: it ends the grant, and the plugin stays installed.

    The account here is the nest's admin (Admin ⊇ User for the plugin verbs),
    which is the app's signed-in account in this fixture.

    The card's settings rows carry no ui.yaml id (``bridges.md`` § Element
    IDs: the owner's 2026-10-16 ruling reuses ``bridge-card`` with no new id),
    so the settings edit is not driven here; its write through
    ``fauna.plugins.set_settings`` is proven by tui's unit test and the nest's
    ``plugin_runner.rs``.
    """
    app = plugin_admin_app
    bridges, ca = app.bridges, app.connected_apps
    url = plugin_publisher["document_url"]
    _install_from_the_app(app, plugin_publisher)

    # ── The card: unbound, it offers Connect ──
    bridges.navigate()
    assert bridges.wait_for_plugin_bound(url, False), (
        f"the Bridges page shows no settings card for {url!r}. "
        f"page error: {bridges.page_error_text(timeout=0)!r}; "
        f"the nest offers: {json.dumps(_available(plugin_nest), default=str)}"
    )
    grants_before = _grant_ids(plugin_nest)

    # ── Connect: the bind card waits in the tray, approved there ──
    bridges.press_plugin_card(url)
    error = bridges.page_error_text(timeout=2.0)
    assert not error, f"Connect was refused: {error!r}"
    ca.navigate()
    i = ca.wait_for_request_from(url)
    assert i is not None, (
        "the bind card never reached Connected apps → Requests. "
        f"error: {ca.current_error_text()!r}"
    )
    ca.approve_request(i)

    # ── The roster: connected, the wasm badge, the link to the card ──
    row = ca.wait_for_item_status(_NAME, "connected")
    available = _available(plugin_nest)
    assert row is not None, (
        f"the roster never showed {_NAME!r} connected. "
        f"error: {ca.current_error_text()!r}; the nest offers: "
        f"{json.dumps(available, default=str)}"
    )
    assert S.connected_apps.class_wasm in ca.item_text(row), ca.item_text(row)
    assert ca.has_settings_link(row), "a plugin's row links to its settings card"
    assert [p.get("binding_principal_id") is not None for p in available] == [True], (
        f"the nest holds no binding row for the account: {json.dumps(available, default=str)}"
    )
    minted = _grant_ids(plugin_nest) - grants_before
    assert minted, "the approve minted no capability grant to the plugin's key"

    # ── The link opens the card, which now reads bound ──
    ca.open_settings_link(row)
    assert bridges.is_page_visible(), "the settings link did not open the Bridges page"
    assert bridges.wait_for_plugin_bound(url, True), (
        f"the card never read bound. page error: {bridges.page_error_text(timeout=0)!r}"
    )

    # ── Disconnect: the grant ends, the plugin stays installed ──
    bridges.press_plugin_card(url)
    assert bridges.wait_for_plugin_bound(url, False), (
        f"the card still read bound after Disconnect. "
        f"page error: {bridges.page_error_text(timeout=0)!r}"
    )
    assert not (minted & _grant_ids(plugin_nest)), (
        "the plugin's capability grant outlived the Disconnect"
    )
    assert [p["client_id"] for p in _listed(plugin_nest)] == [url], (
        "Disconnect ends one account's binding, never the install"
    )
    assert ca.wait_for_no_item_containing(_NAME), "the roster still lists the plugin"
