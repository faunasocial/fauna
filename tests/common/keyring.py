"""Secret Service (libsecret) item helpers for tier_3 Linux relaunch e2e tests —
against the PRIVATE `gnome-keyring-daemon` a test runs for itself, never the
desktop's.

The default Linux e2e driver isolates each launch with a *fresh per-launch*
``FAUNA_KEYRING_APP=fauna-e2e-agent-<port>`` namespace **and** a fresh
``FAUNA_E2E_CREDENTIAL_DIR`` tmpdir, so no persisted slot survives a force-quit
+ relaunch. The driver's ``use_real_keyring`` mode instead uses a STABLE keyring
namespace + STABLE XDG dirs and a real Secret Service, so the identity trio /
pending-invite / multi-account registry index all persist across a relaunch —
the production launch-routing path (see ``drivers/linux.py`` ``launch``
``use_real_keyring`` branch).

That Secret Service is a real ``gnome-keyring-daemon`` on a private session bus
(``drivers/secret_service.py::PrivateSecretService``), owned by the test's
``LibsecretCredStore`` (``tests/common/cred_store.py``) and handed to the launch
as its bus. **Every helper here takes that bus's address; none of them can open
the ambient session bus.** Until 2026-09-15 they did (``secretstorage.dbus_init``
reads ``DBUS_SESSION_BUS_ADDRESS`` from pytest's own environment — the desktop
bus), which put fixture seeds into the developer's login keyring beside the
app's own writes there, and let the tests' force-quits crash the desktop
daemon: it restarted with the login keyring locked, and every other client
of that keyring lost its secrets until a human logged in. Keeping the
address explicit is what makes that unrepeatable by construction.

The attribute layout mirrors ``client.rs::store_credentials`` /
``CredentialStore``: every item carries ``{application, account,
xdg:schema}`` and a plaintext value, and the app finds items with
``search_items({application, account})`` (a subset match), so injected items are
indistinguishable from app-written ones. Every registry key (``fauna/index``,
``fauna/{actor}/{secret,nest_url,device_id}``) maps to the ``account``
attribute verbatim.

``secretstorage`` / ``jeepney`` are imported lazily inside each function so
importing this module (or the ``common`` package) on a non-Linux machine never
requires them.
"""

import contextlib
import shutil
import uuid

#: libsecret attribute the Rust ``secret_service`` crate stamps on every item it
#: writes (so injected items match the app's ``search_items({application,
#: account})`` shape exactly).
XDG_SCHEMA = "org.freedesktop.Secret.Generic"

#: Sample ``account`` attribute values for the keyring-mechanics tests
#: (``test_private_secret_service.py``) — arbitrary item names, not a shape any
#: app reads: the apps key every registry logical key verbatim
#: (``fauna/index``, ``fauna/{actor}/secret`` …).
ACCT_SECRET_KEY = "secret_key"
ACCT_DEVICE_ID = "device_id"
ACCT_NODE_URL = "node_url"


def secret_service_available() -> bool:
    """True iff this box can run a private Secret Service for a test: the
    installed ``gnome-keyring-daemon`` + ``dbus-daemon``, and the python
    ``secretstorage`` binding the helpers use. A Linux desktop session is NOT
    required — the daemon runs on the test's own bus over a throwaway keyring
    (``drivers/secret_service.py``). Tests skip (``skip_environment``) when this
    is False."""
    try:
        import secretstorage  # noqa: F401
    except Exception:
        return False
    return all(shutil.which(tool) for tool in ("gnome-keyring-daemon", "dbus-daemon"))


@contextlib.contextmanager
def _collection(address: str):
    """The default collection of the Secret Service on the bus at ``address``,
    on a connection closed when the block ends."""
    import secretstorage
    from jeepney.io.blocking import open_dbus_connection

    conn = open_dbus_connection(bus=address)
    try:
        yield secretstorage.get_default_collection(conn)
    finally:
        conn.close()


def unique_namespace(stem: str = "fauna-desktop-e2e") -> str:
    """A fresh, unique ``application=`` namespace for one test. Prefixed with
    ``fauna-desktop`` so it shares the app's ``application`` attribute shape, but
    suffixed with a random token so it can NEVER collide with the real
    ``fauna-desktop`` slot or a concurrent sibling run."""
    return f"{stem}-{uuid.uuid4().hex[:8]}"


def clear_namespace(address: str, prefix: str) -> None:
    """Delete every item under ``prefix`` and its ``prefix-*`` sub-namespaces
    (pending-invite / awaiting-manual-dns / pending-encryption-mode / flag) on
    the service at ``address``. Nothing else shares that service, so this is a
    plain sweep — the hand-drained iterator the ambient-keyring version needed
    (a sibling process deleting an item between the listing and the property
    read) has no race left to tolerate."""
    with _collection(address) as coll:
        for item in list(coll.get_all_items()):
            app = item.get_attributes().get("application", "")
            if app == prefix or app.startswith(prefix + "-"):
                item.delete()


def namespace_accounts(address: str, prefix: str) -> set[str]:
    """The ``account`` attribute of every item in the EXACT ``application=prefix``
    namespace. Exact match, not the ``prefix-*`` sweep :func:`clear_namespace`
    does: the app's own namespace delete (``CredentialStore::
    delete_namespace``) searches ``{application: app}`` alone, and the resume
    slots deliberately live in ``prefix-pending-invite`` &co so a sign-out does
    *not* take them. Asserting over the exact namespace is therefore what a
    factory reset actually promises to empty."""
    with _collection(address) as coll:
        return {
            item.get_attributes().get("account", "")
            for item in coll.get_all_items()
            if item.get_attributes().get("application", "") == prefix
        }


def inject_fields(address: str, prefix: str, fields: dict[str, str]):
    """Write registry logical keys (``fauna/index``, ``fauna/{actor}/secret`` …)
    into the service at ``address`` under ``application=prefix``, exactly as
    the app's ``CredentialStore`` keyring arm writes them (attributes
    {application, account = the logical key verbatim, xdg:schema}, plaintext
    value) so the app's launch reads them back through its registry. Build
    the map with ``common.accounts.single_account_seed`` /
    ``build_registry_seed``."""
    with _collection(address) as coll:
        for account, value in fields.items():
            attrs = {"application": prefix, "account": account, "xdg:schema": XDG_SCHEMA}
            coll.create_item(f"Fauna E2E — {account}", attrs, value.encode(), replace=True)


def read_field(address: str, prefix: str, account: str) -> str | None:
    """Read back a single item's value by its ``account`` attribute under
    ``application=prefix`` — the inverse of :func:`inject_fields` and the
    way ``CredentialStore::get`` reads a logical key (``fauna/index`` →
    account ``fauna/index`` verbatim).
    Returns the UTF-8 value, or ``None`` if absent. Reconnect-independent: it
    inspects the persisted keyring the client wrote, with no dependency on the
    nest being reachable — the real-keyring analog of web's ``localStorage``
    read in the account-switcher e2es."""
    with _collection(address) as coll:
        matches = list(coll.search_items({"application": prefix, "account": account}))
        if not matches:
            return None
        return matches[0].get_secret().decode()
