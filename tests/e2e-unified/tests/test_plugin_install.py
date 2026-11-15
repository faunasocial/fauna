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
Disconnects through the roster's one verb. The third drives the bound card's
settings: this account's value of the plugin's one setting, edited on the card
and kept. The last two read the runner's state off the admin list: a plugin
that faulted shows its fault count and last fault, and one whose ``start``
refused is listed stopped with its reason (§ Execution forms → *WASM
components* → *The contract as built*). The last test connects a plugin whose
manifest declares a ``bridge`` block and reads its network row's glyph-headed
title on the Bridges page (§ The manifest → *The ``bridge`` block*).

The plugin is the in-repo hello plugin (``hello_plugin.wat``), componentized
by the ``plugin-fixture-gen`` helper and served — document and module — by
``helpers/client_metadata_server.py`` on a real hostname, which the nest's
``test-hooks`` resolve map points at a loopback port. Every mutation goes
through the app (convention 8); the nest's own list is read alongside to say
which side failed (convention 5).

**Install-and-uninstall test and bind test:** all seven apps (tui the lead).
**Settings, fault, refusal and bridge-glyph tests:** tui first (``testing.md`` § Default
app and nest mode); the other six follow in their trickle-down.
"""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from i18n.strings import S
from helpers.plugin_publisher import (
    BRIDGE_BLOCK as _BRIDGE_BLOCK,
    BRIDGE_GLYPH as _BRIDGE_GLYPH,
    BRIDGE_ID as _BRIDGE_ID,
    HOST as _HOST,
    KIND as _KIND,
    NAME as _NAME,
    REFUSING_REASON as _REFUSING_REASON,
    publish_hello_plugin,
)

pytestmark = [pytest.mark.tier_3]


@pytest.fixture(scope="module")
def plugin_publisher(tmp_path_factory):
    """The publisher's document + module on ``plugins.example``: a signed
    manifest whose ``execution`` pins the hello plugin's digest, one kind,
    one setting, no outbound hosts (``helpers/plugin_publisher.py``)."""
    publisher = publish_hello_plugin(tmp_path_factory.mktemp("plugin-publisher"))
    yield publisher
    publisher["server"].close()


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
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
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
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.standalone_only
@pytest.mark.feature("bridges", "connected-apps")
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

    The bound card's settings edit is the next test's
    (``test_a_bound_plugins_card_keeps_a_setting_edit``).
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
        f"the card never read bound. page error: {bridges.page_error_text(timeout=0)!r}; "
        f"{bridges.plugin_cards_diagnosis()}"
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


def _poll(read, want, budget: float = 15.0):
    """Deadline poll until ``read()`` equals ``want``; returns the last read
    (convention 14 — never a settle-sleep)."""
    deadline = time.monotonic() + budget
    got = read()
    while got != want and time.monotonic() < deadline:
        time.sleep(0.3)
        got = read()
    return got


def _greeting(nest) -> object:
    """This account's ``greeting`` value as the nest serves it on
    ``fauna.plugins.available`` — the schema's default until the account sets one."""
    for plugin in _available(nest):
        for setting in plugin.get("settings") or []:
            if setting.get("key") == "greeting":
                return setting.get("value")
    return None


def _connect_from_the_card(app, url: str) -> None:
    """Connect from the plugin's card and approve the bind card in Connected
    apps — the bind test's journey, which the settings test starts from."""
    bridges, ca = app.bridges, app.connected_apps
    bridges.navigate()
    assert bridges.wait_for_plugin_bound(url, False), (
        f"the Bridges page shows no settings card for {url!r}. "
        f"page error: {bridges.page_error_text(timeout=0)!r}"
    )
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
    bridges.navigate()
    assert bridges.wait_for_plugin_bound(url, True), (
        f"the card never read bound. page error: {bridges.page_error_text(timeout=0)!r}; "
        f"{bridges.plugin_cards_diagnosis()}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.standalone_only
@pytest.mark.feature("bridges")
def test_a_bound_plugins_card_keeps_a_setting_edit(
    plugin_admin_app, plugin_nest, plugin_publisher
):
    """``third-party.md`` § The principal model → *Hosted principals*: once
    bound, the plugin's card paints the plugin's ``settings_schema`` with this
    account's values, through the one settings renderer a linked bridge's card
    uses (``bridge-setting[<key>]``, ``bridges.md`` § Bridge settings). The
    hello plugin declares one string setting, ``greeting`` (default ``hi``):
    the edit commits through ``fauna.plugins.set_settings``, the nest serves
    it back on ``fauna.plugins.available``, and the card still reads it after
    leaving the page."""
    app = plugin_admin_app
    bridges = app.bridges
    url = plugin_publisher["document_url"]
    _install_from_the_app(app, plugin_publisher)
    _connect_from_the_card(app, url)

    # ── The bound card carries the setting, at the schema's default ──
    assert bridges.plugin_has_setting(url, "greeting"), (
        "a bound plugin's card paints its settings: "
        f"{app.driver.diagnose('bridge-setting[greeting]')}"
    )
    assert _greeting(plugin_nest) == "hi"
    assert bridges.plugin_setting_text(url, "greeting") == "hi", (
        app.driver.diagnose("bridge-setting[greeting]")
    )

    # ── The edit saves on commit, with no save control ──
    bridges.commit_plugin_setting(url, "greeting", "hello there")
    assert _poll(lambda: _greeting(plugin_nest), "hello there") == "hello there", (
        f"the greeting should save; the nest has {_greeting(plugin_nest)!r}. "
        f"page error: {bridges.page_error_text(timeout=0)!r}"
    )

    # ── Kept: the card repaints it from the nest after leaving the page ──
    app.driver.navigate_to("feed")
    bridges.navigate()
    assert bridges.wait_for_plugin_bound(url, True), (
        f"the card no longer read bound. page error: {bridges.page_error_text(timeout=0)!r}"
    )
    assert _poll(lambda: bridges.plugin_setting_text(url, "greeting"), "hello there") == (
        "hello there"
    ), app.driver.diagnose("bridge-setting[greeting]")


# ── The runner's state on the admin list (§ The contract as built) ──

FAULT_APEX = "plugin-fault.example"
SPIN_PATH = "/spin/"


@pytest.fixture(scope="module")
def faulting_publisher(tmp_path_factory):
    """The hello plugin declaring one ingress path, ``/spin/``, whose handler
    spins until the per-call fuel budget ends it — the fixture's one fault."""
    publisher = publish_hello_plugin(
        tmp_path_factory.mktemp("plugin-fault-publisher"), ingress=[{"path": SPIN_PATH}]
    )
    yield publisher
    publisher["server"].close()


@pytest.fixture(scope="module")
def refusing_publisher(tmp_path_factory):
    """The variant whose ``start`` answers ``Err(REFUSING_REASON)``."""
    publisher = publish_hello_plugin(
        tmp_path_factory.mktemp("plugin-refusing-publisher"), refusing=True
    )
    yield publisher
    publisher["server"].close()


@pytest.fixture
def faulting_nest(nest_binary, tmp_path_factory, faulting_publisher):
    """Claimed onto ``FAULT_APEX``, so the plugin's path answers on the nest's
    own host (as ``tests/api/test_plugin_ingress.py``)."""
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary,
        tmp_path_factory,
        "plugin-fault",
        claim_domain=FAULT_APEX,
        extra_env=faulting_publisher["server"].nest_env(),
    )
    yield nest
    cleanup()


@pytest.fixture
def refusing_nest(nest_binary, tmp_path_factory, refusing_publisher):
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary,
        tmp_path_factory,
        "plugin-refusing",
        extra_env=refusing_publisher["server"].nest_env(),
    )
    yield nest
    cleanup()


@pytest.fixture
def faulting_spa_url(spa_proxy_for, faulting_nest):
    return spa_proxy_for(faulting_nest["url"])


@pytest.fixture
def refusing_spa_url(spa_proxy_for, refusing_nest):
    return spa_proxy_for(refusing_nest["url"])


@pytest.fixture
def faulting_admin_app(request, app, faulting_nest):
    from conftest import _login_admin_as

    _login_admin_as(
        app,
        request,
        faulting_nest,
        spa_url_fixture="faulting_spa_url",
        fixture_name="faulting_admin_app",
    )
    return app


@pytest.fixture
def refusing_admin_app(request, app, refusing_nest):
    from conftest import _login_admin_as

    _login_admin_as(
        app,
        request,
        refusing_nest,
        spa_url_fixture="refusing_spa_url",
        fixture_name="refusing_admin_app",
    )
    return app


def _ingress_get(nest, host: str, path: str) -> int:
    """One GET on the nest with an explicit ``Host``; its status."""
    req = urllib.request.Request(
        nest["url"].rstrip("/") + path, method="GET", headers={"Host": host}
    )
    try:
        return urllib.request.urlopen(req, timeout=60).status
    except urllib.error.HTTPError as e:
        return e.code


def _status_state_word(state: str) -> str:
    return {
        "running": S.bridges.plugin_state_running,
        "restarting": S.bridges.plugin_state_restarting,
        "stopped": S.bridges.plugin_state_stopped,
    }.get(state, state)


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.standalone_only
@pytest.mark.feature("bridges")
def test_a_faulted_plugin_shows_its_fault_count_and_last_fault(
    faulting_admin_app, faulting_nest, faulting_publisher
):
    """A ``PluginFault`` ends the instance; the runner counts it, lists the
    plugin ``restarting`` while it waits its backoff and re-instantiates it,
    and ``fauna.plugins.list`` keeps the count and the last fault so the admin
    can decide whether to uninstall (``bins/fauna-nest/src/plugin_runner.rs``).
    The backoff starts at one second, so whether a reader catches
    ``restarting`` or the ``running`` after it is a matter of timing: the test
    asserts what stays — the count and the last fault — and that the state
    line in words matches the state the leaf carries, whichever it is
    (convention 14)."""
    app = faulting_admin_app
    bridges = app.bridges
    _install_from_the_app(app, faulting_publisher)
    bridges.navigate()
    row = bridges.wait_for_plugin(_NAME, state="running")
    assert row is not None, (
        f"the plugin never listed running. page error: {bridges.page_error_text(timeout=0)!r}; "
        f"the nest lists: {json.dumps(_listed(faulting_nest), default=str)}"
    )
    assert S.bridges.plugin_faults(count="1") not in bridges.plugin_status_text(row)

    # ── The fault: the declared path's handler runs out of fuel ──
    code = _ingress_get(faulting_nest, FAULT_APEX, SPIN_PATH)
    assert code == 502, f"a faulted ingress call answers 502, got {code}"

    def _status() -> dict:
        listed = _listed(faulting_nest)
        return listed[0]["status"] if listed else {}

    assert _poll(lambda: _status().get("faults"), 1) == 1, (
        f"the nest never counted the fault: {json.dumps(_status(), default=str)}"
    )
    last_fault = _status().get("last_fault")
    assert last_fault, f"the nest keeps no last fault: {json.dumps(_status(), default=str)}"

    # ── The admin list shows it ──
    want = (S.bridges.plugin_faults(count="1"), S.bridges.plugin_last_fault(text=last_fault))
    deadline = time.monotonic() + 30.0
    text = ""
    while True:
        row = bridges.plugin_index_named(_NAME)
        text = bridges.plugin_status_text(row) if row is not None else ""
        if all(w in text for w in want) or time.monotonic() >= deadline:
            break
        bridges.navigate()
    assert all(w in text for w in want), (
        f"the status never showed the fault: {text!r}; "
        f"the nest says {json.dumps(_status(), default=str)}"
    )
    state = bridges.plugin_state(row)
    assert state in ("restarting", "running"), f"a faulted plugin restarts, got {state!r}"
    assert text.splitlines()[0] == _status_state_word(state), (
        f"the state line {text.splitlines()[0]!r} does not say {state!r}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.standalone_only
@pytest.mark.feature("bridges")
def test_a_plugin_that_refused_to_start_is_listed_stopped_with_its_reason(
    refusing_admin_app, refusing_nest, refusing_publisher
):
    """A ``start`` that answers ``Err(msg)`` is the plugin's own refusal: the
    runner lists it ``stopped`` with ``msg`` as its reason and never retries
    it (``plugin_runner.rs``'s module docs). The refusing fixture variant does
    the hello plugin's whole ``start`` and then refuses."""
    app = refusing_admin_app
    bridges = app.bridges
    _install_from_the_app(app, refusing_publisher)
    bridges.navigate()
    row = bridges.wait_for_plugin(_NAME, state="stopped")
    listed = _listed(refusing_nest)
    assert row is not None, (
        f"the plugin never listed stopped. page error: {bridges.page_error_text(timeout=0)!r}; "
        f"the nest lists: {json.dumps(listed, default=str)}"
    )
    assert [p["status"].get("reason") for p in listed] == [_REFUSING_REASON], listed
    text = bridges.plugin_status_text(row)
    assert text.splitlines()[0] == S.bridges.plugin_state_stopped, text
    assert S.bridges.plugin_reason(text=_REFUSING_REASON) in text, text
    assert listed[0]["status"].get("faults", 0) == 0, "a refusal is not a fault"


# ── A consented bridge app's network row (§ The manifest → *The `bridge` block*) ──

# `SourceGlyph::Globe`'s emoji (`libs/fauna-core/src/source_glyph.rs`), which
# the shared `bridge_card_title` puts before a consented bridge's name.
GLOBE = "\N{GLOBE WITH MERIDIANS}"


@pytest.fixture(scope="module")
def bridge_publisher(tmp_path_factory):
    """The hello plugin whose signed manifest declares a ``bridge`` block with
    the ``globe`` glyph (``helpers/plugin_publisher.py``'s ``BRIDGE_BLOCK``)."""
    publisher = publish_hello_plugin(
        tmp_path_factory.mktemp("plugin-bridge-publisher"), bridge=_BRIDGE_BLOCK
    )
    yield publisher
    publisher["server"].close()


@pytest.fixture
def bridge_nest(nest_binary, tmp_path_factory, bridge_publisher):
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary,
        tmp_path_factory,
        "plugin-bridge",
        extra_env=bridge_publisher["server"].nest_env(),
    )
    yield nest
    cleanup()


@pytest.fixture
def bridge_spa_url(spa_proxy_for, bridge_nest):
    return spa_proxy_for(bridge_nest["url"])


@pytest.fixture
def bridge_admin_app(request, app, bridge_nest):
    from conftest import _login_admin_as

    _login_admin_as(
        app,
        request,
        bridge_nest,
        spa_url_fixture="bridge_spa_url",
        fixture_name="bridge_admin_app",
    )
    return app


def _bridge_rows(nest) -> list[dict]:
    """The nest's own ``fauna.bridges.list`` rows for the signed-in account."""
    return _as_owner(nest, "fauna.bridges.list")["bridges"]


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.standalone_only
@pytest.mark.feature("bridges")
def test_a_connected_bridge_app_is_listed_as_a_network_row_with_its_glyph(
    bridge_admin_app, bridge_nest, bridge_publisher
):
    """``third-party.md`` § Implementation status today: a consent carries the
    document's ``bridge`` block to the account's roster, ``fauna.bridges.list``
    lists the consented bridge as a network row carrying its declared
    ``glyph``, and the Bridges page paints that glyph at the head of the row's
    title (``bridge-card-name``) through the shared ``bridge_card_title``. The
    bridge app here is the hello plugin declaring a ``bridge`` block, installed
    and connected through the app (the bind test's journey); before the
    connect no such row exists."""
    app = bridge_admin_app
    bridges = app.bridges
    url = bridge_publisher["document_url"]
    _install_from_the_app(app, bridge_publisher)

    # ── Before the consent: no network row for the bridge ──
    assert not [r for r in _bridge_rows(bridge_nest) if r.get("id") == _BRIDGE_ID], (
        "an installed but unconnected bridge app is already a network row"
    )
    bridges.navigate()
    assert not [n for n in bridges.card_names() if n.startswith(GLOBE)], (
        f"a glyph-headed card before any consent: {bridges.card_names()!r}"
    )

    # ── Connect and approve: the roster now carries the bridge ──
    _connect_from_the_card(app, url)
    rows = [r for r in _bridge_rows(bridge_nest) if r.get("id") == _BRIDGE_ID]
    assert len(rows) == 1, (
        f"the nest lists no network row for the consented bridge: "
        f"{json.dumps(_bridge_rows(bridge_nest), default=str)}"
    )
    assert rows[0].get("glyph") == _BRIDGE_GLYPH, rows[0]
    assert rows[0].get("name") == _NAME, rows[0]

    # ── The page: a network row titled with its glyph before its name ──
    title = f"{GLOBE} {_NAME}"
    assert bridges.wait_for_card_named(title, bridge_id=_BRIDGE_ID), (
        f"no card titled {title!r}; the page paints {bridges.card_names()!r}. "
        f"page error: {bridges.page_error_text(timeout=0)!r}; "
        f"{app.driver.diagnose('bridge-card-name')}"
    )
    # The plugin's own settings card keeps its bare name beside it.
    assert _NAME in bridges.card_names(), bridges.card_names()
