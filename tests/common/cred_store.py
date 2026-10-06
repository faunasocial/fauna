"""Per-app persistent-credential adapters for launch-routing e2e tests.

The launch-routing cases (``docs/goal/behavior/onboarding.md`` § App-launch
routing) all hinge on one precondition the default e2e drivers deliberately
destroy: an identity trio that **survives a force-quit + relaunch**. Each driver
isolates every launch with a fresh per-launch keyring namespace *and* a fresh
per-launch credential dir, so nothing persists.

Both direct-Rust desktop apps read their credentials through the same shared
store (``libs/fauna-credential-store``), which has two backends:

* **libsecret** (production) — the freedesktop Secret Service, namespaced by an
  ``application`` attribute. The linux driver's ``use_real_keyring`` mode points
  the app at a stable namespace on a real ``gnome-keyring-daemon`` the test
  runs privately for itself (``LibsecretCredStore``), so the trio persists —
  never the developer's desktop keyring, which a test's force-quit used to
  crash machine-wide (2026-09-15). Needs the daemon installed, not a desktop.
* **file** (``FAUNA_E2E_CREDENTIAL_DIR``) — a 0600 ``{dir}/{app}.json`` map, the
  documented fallback for keyring-unavailable boxes. Pointing a client at a
  *stable* dir gives the same persistence with no D-Bus dependency at all.

Both backends key every registry logical key verbatim (``fauna/index``,
``fauna/{actor}/secret`` …), and the registry shape is the ONLY shape an app
reads (2026-09-24, ``long-term-store.md`` § Downgrade mirror + abandoned-append
recovery): the pre-registry single slot is neither read at launch nor migrated
into account #1 any more. So an injected identity is the exact state a real
onboarding's ``persist_confirmed_identity`` + ``persist_logged_in`` leave
behind (``common.accounts.single_account_seed``), and the launch machine's
``RegistryLaunchPersistence`` reads it back verbatim.

A [`CredStore`] hands a test the two things it needs — *inject an identity* and
*build the launch config that reads it back* — without the test knowing which
backend is underneath. That is what lets one launch-routing module run on both
apps (priority #1: no per-app test divergence).
"""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import uuid
from pathlib import Path

from . import keyring as libsecret
from .accounts import single_account_seed

DEFAULT_DEVICE_ID = "smoke-e2e-device"

#: ``fauna_credential_store::ACCOUNT_STORE_NAMESPACE`` — the shared namespace
#: holding this machine's T10 store writer key and each account's
#: ``principal_bundle`` slots, distinct from any app's own namespace
#: (``libs/fauna-credential-store/src/lib.rs``, ``long-term-store.md`` §
#: Cleanup contract). Native Rust is the single source of truth for the
#: string; this is a name-matched python mirror, same convention as
#: ``DEFAULT_DEVICE_ID`` above.
ACCOUNT_STORE_NAMESPACE = "fauna-account-store"

#: The key prefix the account-store namespace's rows carry on a phone, where
#: there is no native Rust keyring arm and the namespace rides the foreign seam
#: into the app's own flat store beside the identity rows
#: (``fauna_credential_store::foreign_key`` — ``{namespace}/{account}``; no
#: ``FAUNA_KEYRING_APP`` override on either phone, so the bare namespace).
ACCOUNT_STORE_FOREIGN_PREFIX = f"{ACCOUNT_STORE_NAMESPACE}/"

#: ``fauna_client_accounts::device_id::INSTALL_DEVICE_SECRET`` — the install
#: device secret's row on an app that keeps it in its one secret store (windows;
#: apple). Install-scoped: it names no account, and a sign-out never erases it
#: (``sync-agent-credentials.md`` § Credential model, the RULED 2026-09-20
#: block; ``account-scoping.md`` § Erasure follows scope — "install-scoped
#: state survives"). So it is never an identity survivor: a test grading "no
#: identity survived" subtracts it, the way web's ``stored_accounts`` scopes to
#: ``fauna/``. Name-matched python mirror, same convention as the two above.
INSTALL_DEVICE_SECRET_SLOT = "install/device_secret"


def account_store_namespace(keyring_app: str | None) -> str:
    """The account-store namespace a launch with ``FAUNA_KEYRING_APP=keyring_app``
    resolves: ``{keyring_app}-account-store``, or the bare
    :data:`ACCOUNT_STORE_NAMESPACE` when no override is set (apple).

    The python mirror of ``fauna_credential_store::apply_namespace_override``,
    and the harness's ONE spelling of it: :func:`attach_account_store` reads the
    namespace through it and the linux/tui drivers' principal-slot carry
    (``drivers/http_bridge.py``) writes through it, so the file a test inspects
    and the file the carry restores cannot drift apart."""
    return f"{keyring_app}-account-store" if keyring_app else ACCOUNT_STORE_NAMESPACE


def account_store_location(client: str, cred_dir: str, keyring_app: str | None) -> tuple[str, str]:
    """Where a launch's ``fauna-account-store`` entries physically rest under the
    e2e credential dir: ``(file, key prefix)``, every entry keyed
    ``{prefix}{account}`` inside ``file``.

    The harness's ONE table of it — :func:`attach_account_store`'s apple adapters
    read through it and the drivers' principal-slot carry
    (``drivers/http_bridge.py``) harvests and restores through it:

    - **macOS** — the Rust crate's File backend, a sibling
      ``fauna-account-store.json`` (no ``FAUNA_KEYRING_APP`` on apple, so the bare
      namespace; ``keyring_app`` names the Swift store, not this one).
    - **iOS** — no native Rust keyring arm, so the namespace rides the foreign seam
      into the app's own ``keychain.json``, each key prefixed with the namespace
      (``fauna_credential_store::foreign_key``), beside the identity rows.
    - **linux / tui / windows** — the File backend's ``{namespace}.json`` under the
      derived override (:func:`account_store_namespace`).
    - **android** — iOS's foreign-seam shape, prefix and all, but in the
      on-device e2e credential file, which has no host path: no ``cred_dir``
      reaches it, so it has no row here and :class:`AndroidAccountStoreCredStore`
      reads it over the bridge, filtered by :data:`ACCOUNT_STORE_FOREIGN_PREFIX`."""
    if client == "ios":
        return os.path.join(cred_dir, "keychain.json"), ACCOUNT_STORE_FOREIGN_PREFIX
    if client == "macos":
        return os.path.join(cred_dir, f"{ACCOUNT_STORE_NAMESPACE}.json"), ""
    return os.path.join(cred_dir, f"{account_store_namespace(keyring_app)}.json"), ""


DEFAULT_WEB_HANDLE = "e2e-user"


class CredStore:
    """One test's isolated, *persistent* credential namespace."""

    #: Client this store drives.
    client: str

    def inject_identity(self, *, secret_hex: str, node_url: str | None = None) -> None:
        """Write one signed-in identity in the registry shape (``fauna/index``
        + the per-actor slots) so the next launch reads it back. Omit
        ``node_url`` to leave that slot absent (the "identity, no nest_url"
        hydration branch)."""
        raise NotImplementedError

    def inject_raw(self, fields: dict[str, str]) -> None:
        """Write logical keys **verbatim**, beside whatever is already stored —
        for a case whose subject is a store the app did not write itself, such
        as an account index this build cannot read
        (``version-compatibility.md`` § 5 item 9). Call after
        ``inject_identity``: it merges, never replaces. Not implemented on a
        backend no such case has needed yet — it raises rather than pretending."""
        raise NotImplementedError(f"inject_raw is not wired for {self.client}")

    def launch_config(self, app_path: str, node_url: str) -> dict:
        """Driver config pinning this store's namespace across relaunches."""
        raise NotImplementedError

    def clear(self) -> None:
        """Sweep the namespace. Safe to call twice."""
        raise NotImplementedError

    def close(self) -> None:
        """Release whatever the store runs for itself — linux's private Secret
        Service daemon. A no-op for the file backends. Safe to call twice; the
        harness calls it after the final ``clear()``."""

    def stored_accounts(self) -> set[str]:
        """Every credential slot currently in the namespace, by its native
        ``account`` name (``fauna/index``, ``fauna/{actor}/secret``, …). Empty exactly when the namespace is empty
        — what a factory reset must leave behind. Naming the survivors is what
        lets a reset assertion diagnose itself."""
        raise NotImplementedError


class LibsecretCredStore(CredStore):
    """A real Secret Service, stable namespace — the production launch path on
    linux (libsecret over D-Bus to a real ``gnome-keyring-daemon``).

    The service is **this store's own**: a private daemon on a private session
    bus (``drivers/secret_service.py::PrivateSecretService``), started on first
    use over a throwaway, non-interactively unlocked keyring, and handed to
    every launch as its bus through ``launch_config()``'s ``environment``. The
    developer's desktop keyring is never on that bus, so a test's force-quit
    cannot crash it (the 2026-09-15 machine-wide credential outage) and a
    fixture seed cannot land in it. The store outlives the driver, which is
    what makes a ``teardown()`` + ``launch()`` relaunch read the same items
    back; a daemon that dies mid-test is restarted over the same keyring by
    the next helper call. ``close()`` stops it.

    ``attach_cred_store`` builds one in *attached* form (``bus_address`` given):
    it reads the service an already-launched driver was handed and owns
    nothing."""

    client = "linux"

    def __init__(self, namespace: str, xdg_base: str, *, bus_address: str | None = None):
        self._ns = namespace
        self._xdg = xdg_base
        self._service = None
        self._attached_address = bus_address

    def _address(self) -> str:
        if self._attached_address is not None:
            return self._attached_address
        if self._service is None:
            from drivers.secret_service import PrivateSecretService

            self._service = PrivateSecretService().start()
        else:
            self._service.ensure_running()
        return self._service.address

    @property
    def service(self):
        """The owned ``PrivateSecretService`` (``None`` when attached, or before
        first use) — for a test asserting on the daemon itself, e.g. that a
        force-quit never killed it (``daemon_exits``)."""
        return self._service

    def inject_identity(self, *, secret_hex: str, node_url: str | None = None) -> None:
        libsecret.inject_fields(
            self._address(), self._ns,
            single_account_seed(secret_hex, nest_url=node_url, device_id=DEFAULT_DEVICE_ID),
        )

    def read_field(self, account: str) -> str | None:
        """One slot's value by its native ``account`` name (``fauna/index``,
        ``fauna/{actor}/secret``, …), ``None`` when absent — the keyring twin of
        ``FileCredStore.read_map`` for an assertion about a slot's VALUE."""
        return libsecret.read_field(self._address(), self._ns, account)

    def launch_config(self, app_path: str, node_url: str) -> dict:
        return {
            "app_path": app_path,
            "url": node_url,
            "use_real_keyring": True,
            "keyring_app": self._ns,
            "xdg_base": self._xdg,
            # The caller-owned-bus carve-out (`drivers/linux.py::_wants_private_bus`):
            # the launch rides the bus this store's private daemon lives on.
            "environment": {"DBUS_SESSION_BUS_ADDRESS": self._address()},
        }

    def clear(self) -> None:
        libsecret.clear_namespace(self._address(), self._ns)

    def close(self) -> None:
        service, self._service = self._service, None
        if service is not None:
            service.stop()

    def stored_accounts(self) -> set[str]:
        return libsecret.namespace_accounts(self._address(), self._ns)


class FileCredStore(CredStore):
    """The shared store's ``FAUNA_E2E_CREDENTIAL_DIR`` file backend, pinned to a
    stable dir. Used by tui: headless-safe, needs no Secret Service, and can
    never touch the user's real credentials because the dir is a tmpdir."""

    client = "tui"

    def __init__(self, namespace: str, creds_dir: str, xdg_base: str):
        self._ns = namespace
        self._dir = Path(creds_dir)
        self._xdg = xdg_base

    def _path(self) -> Path:
        return self._dir / f"{self._ns}.json"

    def read_map(self) -> dict[str, str]:
        """The namespace's whole ``account -> value`` map, ``{}`` when the file
        is missing or unreadable (``fauna_credential_store::cred_file_read``'s
        own contract). For an assertion about a slot's VALUE — the writer key a
        relaunch must keep — which :meth:`stored_accounts` cannot answer."""
        try:
            value = json.loads(self._path().read_text())
        except (OSError, ValueError):
            return {}
        return value if isinstance(value, dict) else {}

    def inject_identity(self, *, secret_hex: str, node_url: str | None = None) -> None:
        self._dir.mkdir(parents=True, exist_ok=True)
        fields = single_account_seed(secret_hex, nest_url=node_url, device_id=DEFAULT_DEVICE_ID)
        path = self._path()
        path.write_text(json.dumps(fields, indent=2))
        os.chmod(path, 0o600)  # the app writes 0600; match it exactly

    def inject_raw(self, fields: dict[str, str]) -> None:
        # The File backend maps every logical key straight to its file key
        # (`drivers/tui.py`'s seed comment), so `fauna/index` lands as
        # `fauna/index` — exactly the blob the registry reads at launch.
        self._dir.mkdir(parents=True, exist_ok=True)
        path = self._path()
        try:
            current = json.loads(path.read_text())
        except (OSError, ValueError):
            current = {}
        current.update(fields)
        path.write_text(json.dumps(current, indent=2))
        os.chmod(path, 0o600)

    def launch_config(self, app_path: str, node_url: str) -> dict:
        return {
            "app_path": app_path,
            "url": node_url,
            "keyring_app": self._ns,
            "credential_dir": str(self._dir),
            "xdg_base": self._xdg,
        }

    def clear(self) -> None:
        with contextlib.suppress(Exception):
            shutil.rmtree(self._dir)

    def stored_accounts(self) -> set[str]:
        try:
            return set(json.loads(self._path().read_text()))
        except Exception:
            return set()


class LinuxFileCredStore(FileCredStore):
    """linux driven through the shared store's file backend instead of the real
    session Secret Service. Only the *pending-invite* slot used to force linux
    onto ``LibsecretCredStore`` for this whole module — it went straight to
    libsecret over D-Bus, bypassing ``FAUNA_E2E_CREDENTIAL_DIR`` entirely. Now
    that it lives on the shared ``AccountRegistry`` (the same store the
    identity trio already uses), linux's pending-invite case is headless-safe
    too — this class is how that gets proven (see
    ``test_smoke_a_pending_invite_survives_force_quit``'s
    ``headless_credential_store`` marker), without changing the *default*
    backend the other cases in this module still use."""

    client = "linux"


class AppleFileCredStore(FileCredStore):
    """macOS / iOS via the shared store's file backend — the same
    ``FAUNA_E2E_CREDENTIAL_DIR`` mechanism linux/tui use, already built with
    this exact parity in mind (``drivers/macos.py`` / ``drivers/ios.py``
    ``launch()``'s ``config["credential_dir"]`` + ``config["seed_credentials"]``
    handling, and their ``KeychainStore.e2eFileURL`` app-side gate). Only the
    filename differs: apple's E2E keychain file is always named
    ``keychain.json`` (fixed, not ``{namespace}.json``) — apple isolates by
    *directory* (a fresh ``credential_dir`` per test), not by filename, so no
    per-namespace name is needed the way a shared multi-app dir would need
    one. There is no real-keyring equivalent to fall back to on apple (no
    ``use_real_keyring`` in either apple driver), so this is the *only*
    backend for both apple apps — ``requires_secret_service`` already
    returns `False` for anything but linux, and both ``make_cred_store`` and
    ``make_file_backed_cred_store`` return the same store for macos/ios."""

    def __init__(self, client: str, creds_dir: str):
        self.client = client
        self._dir = Path(creds_dir)

    def _path(self) -> Path:
        return self._dir / "keychain.json"

    def launch_config(self, app_path: str, node_url: str) -> dict:
        return {"app_path": app_path, "url": node_url, "credential_dir": str(self._dir)}


class WindowsFileCredStore(FileCredStore):
    """windows via the shared file backend — the same
    ``FAUNA_E2E_CREDENTIAL_DIR``/``{namespace}.json`` mechanism linux/tui use.
    ``FileSecretBackend`` (``apps/fauna-windows/FaunaApp/FaunaApp.Core/Services/
    SecretBackend.cs``) is a deliberate twin of linux's ``cred_file_read``/
    ``cred_file_write``: same path convention, same flat map, keyed by the
    logical key verbatim (``SecretKeyMap`` remaps nothing since the windows leg
    retired the pre-registry single slot). There is no real-Credential-Manager
    equivalent to fall back to (the suite must never write the dev box's real
    Credential Manager), so this is the only backend for windows and
    ``requires_secret_service`` stays ``False``.

    **One thing differs from the tui/linux parent, and it is load-bearing:**
    **``data_dir`` rides in the launch config.** This is the exact analogue of
       linux/tui pinning ``xdg_base``: the harness's ``relaunch()`` is
       ``teardown()`` + ``launch()``, and ``drivers/windows.py``'s ``teardown()``
       rmtree's a driver-*owned* data dir (``_owns_data_dir``) while preserving a
       caller-supplied one. Unpinned, every relaunch would come up on a fresh
       ``%LocalAppData%\\Fauna`` — a *new device*, not the same one restarting,
       which is not the lifecycle these cases assert. Pinning it makes the
       relaunch the same-device restart windows' ``recover()`` already is.
    """

    client = "windows"

    def __init__(self, namespace: str, creds_dir: str, data_dir: str):
        self._ns = namespace
        self._dir = Path(creds_dir)
        self._data_dir = data_dir

    def inject_identity(self, *, secret_hex: str, node_url: str | None = None) -> None:
        # The registry shape, verbatim: windows' file backend keys every logical
        # key as-is, and its launch reads this registry alone.
        self._dir.mkdir(parents=True, exist_ok=True)
        fields = single_account_seed(secret_hex, nest_url=node_url, device_id=DEFAULT_DEVICE_ID)
        path = self._path()
        path.write_text(json.dumps(fields, indent=2))
        os.chmod(path, 0o600)  # the app writes 0600; match it exactly

    def launch_config(self, app_path: str, node_url: str) -> dict:
        return {
            "app_path": app_path,
            "url": node_url,
            "keyring_app": self._ns,
            "credential_dir": str(self._dir),
            "data_dir": self._data_dir,
        }


class AndroidCredStore(CredStore):
    """android's launch-path credential store — the file the on-device bridge
    writes into the app's own ``filesDir`` before it starts the activity
    (``AppLauncher.launchApp``'s ``credentialFile()``; the app reads it through
    its ``FileSecretBackend`` when the ``FAUNA_E2E_CREDENTIAL_FILE`` extra names
    it).

    There is no host-side path to that file: the instrumentation shares the
    app's UID, and the one boundary crossing is the ``/session`` POST. So this
    store is **deferred** where the file backends write at once — it holds the
    seed on the host and hands it to the launch as ``seed_credentials``
    (``launch_config()``), which the bridge writes as ONE complete map. The
    persistence a relaunch needs is the on-device file itself, which the bridge
    keeps forwarding; the harness must therefore not re-send the seed on a
    relaunch (``AndroidLaunchHarness.relaunch``), or it would overwrite what the
    app stored since.

    ``clear()`` empties the pending seed, and an empty seed is still SENT: the
    bridge writes ``{}`` over whatever an earlier test left on the device, which
    is the only sweep a host-side store can do.

    Reading the file back needs a launched driver: the bridge's
    ``GET /credentials`` (``AndroidBridgeDriver.credential_map``) is the one
    host-side view of it. :func:`attach_cred_store` binds one; a store built
    for a launch (:func:`make_cred_store`) has none, and its
    ``stored_accounts``/``read_map`` refuse rather than answer from the
    pending seed, which is not what the device holds."""

    client = "android"

    def __init__(self, driver=None):
        self._seed: dict[str, str] = {}
        #: A launched android driver whose bridge reads the device file back.
        self._driver = driver

    def inject_identity(self, *, secret_hex: str, node_url: str | None = None) -> None:
        # A full overwrite, like the file stores' write — the registry shape,
        # which android's file backend keys verbatim and its launch reads.
        self._seed = single_account_seed(
            secret_hex, nest_url=node_url, device_id=DEFAULT_DEVICE_ID
        )

    def inject_raw(self, fields: dict[str, str]) -> None:
        # Logical keys resolve verbatim on android's file backend, so they
        # merge straight into the seed.
        self._seed.update(fields)

    def launch_config(self, app_path: str, node_url: str) -> dict:
        return {"app_path": app_path, "url": node_url, "seed_credentials": dict(self._seed)}

    def clear(self) -> None:
        self._seed = {}

    def read_map(self) -> dict[str, str]:
        """The device file's whole ``native_key -> value`` map, read live over
        the bridge — the file backends' ``read_map`` twin."""
        if self._driver is None:
            raise NotImplementedError(
                "android's credential file lives in the app's own filesDir with no "
                "host-side path; only a store attached to a launched driver "
                "(attach_cred_store) can read it back, over the bridge"
            )
        return self._driver.credential_map()

    def stored_accounts(self) -> set[str]:
        return set(self.read_map())


class WebCredStore(CredStore):
    """The web SPA's launch-path credential store — browser ``localStorage`` on
    the SPA origin.

    Web is the one app whose credentials live *inside* the running browser,
    not in an external backend a yet-to-launch binary reads. So unlike the native
    adapters (which write libsecret / a file dir with **no driver**, before the
    process boots), this adapter is **driver-backed**: it operates on an
    already-launched web driver, and its "persistence across relaunch" is the
    ``localStorage`` that survives a ``hard_reload()``. A driver ``teardown()``
    discards the whole browser profile (localStorage included), so web has no
    native-style force-quit — the launch/relaunch lifecycle is owned by
    ``WebLaunchHarness`` (``tests/common/launch_harness.py``), not this store.

    It deliberately has **no ``launch_config``** (web launches no binary and has
    no ``app_path``). ``stored_accounts`` is likewise unimplemented here — the
    cross-app sign-out erase lift is its first consumer and owns proving it
    (see below)."""

    client = "web"

    def __init__(self, driver):
        #: An already-launched web driver whose page is on the SPA origin.
        self._driver = driver

    def inject_identity(self, *, secret_hex: str, node_url: str | None = None) -> None:
        # The registry shape (`fauna/index` + per-actor slots), written straight
        # into localStorage — web's `LocalStorageSecretStore` keys every logical
        # key verbatim. Omit the node_url key to leave that slot absent (the
        # "identity, no nest_url" hydration branch), exactly as the
        # file/libsecret backends do; the handle rides in the index entry.
        self.inject_raw(
            single_account_seed(secret_hex, nest_url=node_url, handle=DEFAULT_WEB_HANDLE)
        )

    def inject_raw(self, fields: dict[str, str]) -> None:
        # A straight `localStorage.setItem` per key, `eval_js`'d against the
        # already-launched driver — no wasm involved. Logical keys
        # (`fauna/index` included) are untranslated on this backend
        # (`web_store.rs` maps them to themselves verbatim), so writing them
        # straight into localStorage is byte-for-byte what the registry reads
        # at launch. `setItem` never clears other keys, so this merges with
        # whatever is already there, exactly like the file backend's
        # read-modify-write.
        parts = [
            f"localStorage.setItem({json.dumps(key)}, {json.dumps(value)});"
            for key, value in fields.items()
        ]
        self._driver.eval_js("(() => { " + " ".join(parts) + " return true; })()")

    def launch_config(self, app_path: str, node_url: str) -> dict:
        raise NotImplementedError(
            "web launches no binary and has no app_path; drive the web launch "
            "lifecycle through WebLaunchHarness (tests/common/launch_harness.py)"
        )

    def clear(self) -> None:
        # Wipe the `fauna/` registry namespace (and everything else). The test
        # agent lives on `window`, not localStorage, so a wholesale clear is
        # safe.
        self._driver.eval_js("(() => { localStorage.clear(); return true; })()")

    #: The single-slot *credential* keys (``fauna_secret`` etc.) — no current
    #: build writes them. Kept in the survivor census so a sign-out that
    #: somehow left one behind still reds the erase test.
    WEB_LEGACY_CREDENTIAL_KEYS = ("fauna_secret", "fauna_handle", "fauna_domain", "fauna_tier")

    def stored_accounts(self) -> set[str]:
        """Every credential slot in the browser store: the whole `fauna/`
        AccountRegistry namespace (plus the single-slot credential keys in
        ``WEB_LEGACY_CREDENTIAL_KEYS``).

        Scoped rather than prefix-matched, because `localStorage` is a *shared*
        bucket where the native backends' file is not: other `fauna_`-prefixed
        keys (`fauna_registered`, the persistence-v2 flag) are **not**
        credentials. A blanket prefix match
        would report those as surviving credentials and red an erase that was
        in fact complete.
        """
        keys = json.dumps(list(self.WEB_LEGACY_CREDENTIAL_KEYS))
        return set(
            self._driver.eval_js(
                "Object.keys(localStorage).filter("
                f"k => k.startsWith('fauna/') || {keys}.includes(k))"
            )
        )

    def read_map(self) -> dict[str, str]:
        """Every key the page's ``localStorage`` holds, with its value — the
        read-back of the account runtime's T10 slot, which on web rests in the
        same store as the registry (``LocalStorageSecretStore``, every logical
        key verbatim: the writer key at the bare actor id, the bundle at
        ``{actor}/{suffix}``). The file backends' ``read_map`` twin."""
        return dict(
            self._driver.eval_js(
                "Object.fromEntries(Object.keys(localStorage).map("
                "k => [k, localStorage.getItem(k)]))"
            )
        )


def make_cred_store(client: str, tmp_path, *, driver=None) -> CredStore:
    """Build the persistent-credential adapter for ``client``.

    The native adapters (linux/tui) are backend-rooted under this test's
    ``tmp_path`` with a unique namespace, so a crashed prior run can't leak in
    and this run can't leak out. ``web`` is different: its store is the browser's
    ``localStorage``, so pass ``driver=<already-launched web driver>`` and it
    attaches to that (``tmp_path`` is unused for web)."""
    if client == "web":
        if driver is None:
            raise ValueError(
                "web CredStore is driver-backed — pass driver=<launched web "
                "driver>; its localStorage on the SPA origin IS the store"
            )
        return WebCredStore(driver)
    ns = f"fauna-launch-smoke-{uuid.uuid4().hex[:8]}"
    xdg_base = str(tmp_path / "xdg")
    if client == "linux":
        return LibsecretCredStore(ns, xdg_base)
    if client == "tui":
        return FileCredStore(ns, str(tmp_path / "creds"), xdg_base)
    if client in ("macos", "ios"):
        # No real-keyring backend exists on apple (see AppleFileCredStore) — the
        # file backend is the only adapter, same as tui.
        return AppleFileCredStore(client, str(tmp_path / "creds"))
    if client == "windows":
        # Same reasoning as apple: no real-vault backend to fall back to, so the
        # file backend is the only adapter (see WindowsFileCredStore).
        return WindowsFileCredStore(ns, str(tmp_path / "creds"), str(tmp_path / "data"))
    if client == "android":
        # The on-device file the bridge writes is the only backend, and it is
        # isolated per device rather than per tmp dir (see AndroidCredStore).
        return AndroidCredStore()
    raise ValueError(f"no persistent-credential adapter for client {client!r}")


def attach_cred_store(client: str, driver) -> CredStore:
    """Bind a :class:`CredStore` to the namespace an **already-launched** driver
    is using, instead of dictating one to a launch the test controls.

    :func:`make_cred_store` is the launch-routing shape: the test mints a stable
    namespace, hands it to ``launch()`` via ``launch_config()``, and the identity
    survives a force-quit because the test owns the store. That shape cannot serve
    a test on the shared ``app`` / ``logged_in_app`` fixture — those drivers are
    session-scoped and launched themselves long before the test ran, each with a
    *per-launch* namespace (rule 10 isolation: fresh keyring app, fresh credential
    dir). The store therefore has to be **discovered from the driver**, which is
    what this does, and it is what lets one module assert the sign-out erase on
    every app rather than only the two that build their own launch.

    Every native app resolves to the same shared file backend the app itself
    writes (``{FAUNA_E2E_CREDENTIAL_DIR}/{FAUNA_KEYRING_APP}.json``, apple's fixed
    ``keychain.json``), named by the cross-driver ``_resolved_credential_dir`` /
    ``_resolved_keyring_app`` pair every driver records at launch. linux in
    ``use_real_keyring`` mode has no credential dir (libsecret is the sole
    backend), and falls back to the libsecret adapter on the namespace it pinned.

    ⚠ The returned store reads the *live* namespace, so ``clear()`` on it would
    erase the running app's credentials out from under it. Attach to **read**
    (``stored_accounts``); drive the erase through the product's own UI.
    """
    if client == "web":
        # Driver-backed already — its localStorage IS the store, and the driver
        # it attaches to is the launched one by construction.
        return WebCredStore(driver)

    if client == "android":
        # The file is on-device (AppLauncher.launchApp's credentialFile()), so
        # there is no path to discover: the store reads it through the driver's
        # bridge read-back, the read half of the `seed_credentials` crossing.
        return AndroidCredStore(driver=driver)

    cred_dir = getattr(driver, "_resolved_credential_dir", None)
    keyring_app = getattr(driver, "_resolved_keyring_app", None)
    if not keyring_app:
        raise RuntimeError(
            f"driver for {client!r} recorded no `_resolved_keyring_app` — either it "
            "has not launched yet, or it does not implement the cross-driver "
            "resolved-store contract (drivers/linux.py, tui.py, macos.py, ios.py, "
            "windows.py all set the pair in `launch()`)"
        )

    if cred_dir is None:
        # linux `use_real_keyring`: the trio lives in the Secret Service on the
        # launch's bus under the pinned `application` namespace, with no file
        # backend at all. Attach to that bus — the driver refused to launch
        # without one — rather than starting a second daemon of our own.
        if client != "linux":
            raise RuntimeError(
                f"{client!r} launched with no credential dir and has no keyring "
                "backend to fall back to"
            )
        return _attached_libsecret_store(driver, keyring_app)

    if client in ("macos", "ios"):
        return AppleFileCredStore(client, cred_dir)
    if client == "windows":
        return WindowsFileCredStore(
            keyring_app, cred_dir, getattr(driver, "_resolved_data_dir", "")
        )
    if client == "linux":
        return LinuxFileCredStore(keyring_app, cred_dir, getattr(driver, "_resolved_xdg_base", ""))
    if client == "tui":
        return FileCredStore(keyring_app, cred_dir, getattr(driver, "_resolved_xdg_base", ""))
    raise ValueError(f"no persistent-credential adapter for client {client!r}")


class AppleAccountStoreCredStore(CredStore):
    """macOS's shared account-store namespace: a file SEPARATE from the app's
    own identity store.

    On macOS the app's own identity lives in Swift's ``keychain.json`` (see
    ``AppleFileCredStore``) — a mechanism outside ``fauna-credential-store``
    entirely. The shared ``fauna-account-store`` namespace (writer key +
    principal bundle) is minted through the Rust crate instead, which has a
    native macOS Keychain arm; under e2e ``FAUNA_E2E_CREDENTIAL_DIR`` routes
    it to the **File** backend regardless (``resolve_backend``), landing at
    ``{cred_dir}/fauna-account-store.json`` — a second, sibling file. No
    ``FAUNA_KEYRING_APP`` override applies on apple (neither apple driver
    sets it), so the filename is the bare constant, not a derived one."""

    client = "macos"

    def __init__(self, creds_dir: str):
        self._dir = Path(creds_dir)

    def _path(self) -> Path:
        return Path(account_store_location(self.client, str(self._dir), None)[0])

    def read_map(self) -> dict[str, str]:
        """The namespace's whole ``account -> value`` map, ``{}`` when missing —
        :meth:`FileCredStore.read_map`'s contract, for a slot's VALUE."""
        try:
            value = json.loads(self._path().read_text())
        except (OSError, ValueError):
            return {}
        return value if isinstance(value, dict) else {}

    def stored_accounts(self) -> set[str]:
        return set(self.read_map())


class AppleForeignAccountStoreCredStore(CredStore):
    """iOS's shared account-store rows: the SAME ``keychain.json`` the app's
    own identity lives in.

    iOS has no native Rust keyring arm (``HAS_NATIVE_KEYRING_ARM`` is
    ``false`` there), so both the app's own namespace and the account-store
    namespace ride the Foreign seam over the same installed Swift Keychain
    store, each key prefixed ``{namespace}/{account}``
    (``fauna_credential_store::foreign_key``). This class reads the same file
    ``AppleFileCredStore`` does and filters to the ``fauna-account-store/``
    prefix, so a sign-out that erases the app's own rows but leaves the
    account-store's behind is still caught even though both live in one
    file."""

    client = "ios"

    def __init__(self, creds_dir: str):
        self._dir = Path(creds_dir)

    def _path(self) -> Path:
        return Path(account_store_location(self.client, str(self._dir), None)[0])

    def read_map(self) -> dict[str, str]:
        """The account-store rows' ``account -> value`` map with the namespace
        prefix stripped — the same shape :meth:`FileCredStore.read_map` gives on
        the platforms where the namespace is a file of its own. ``{}`` when the
        file is missing or unreadable."""
        prefix = account_store_location(self.client, str(self._dir), None)[1]
        try:
            value = json.loads(self._path().read_text())
        except (OSError, ValueError):
            return {}
        if not isinstance(value, dict):
            return {}
        return {k[len(prefix) :]: v for k, v in value.items() if k.startswith(prefix)}

    def stored_accounts(self) -> set[str]:
        return set(self.read_map())


class AndroidAccountStoreCredStore(CredStore):
    """android's shared account-store rows: the foreign-seam rows of the SAME
    on-device e2e credential file the app's own identity lives in.

    android has no native Rust keyring arm either, so the namespace rides the
    foreign seam over the one ``FfiSecretStore`` the app lends shared Rust
    (``LaunchModule.provideSecretStore``) — under e2e the ``FileSecretBackend``
    over ``FAUNA_E2E_CREDENTIAL_FILE`` — each key prefixed
    :data:`ACCOUNT_STORE_FOREIGN_PREFIX`. iOS's shape exactly
    (:class:`AppleForeignAccountStoreCredStore`); only the read differs: the file
    is on-device, so the whole map comes over the bridge's ``GET /credentials``
    (:class:`AndroidCredStore`) and is filtered here."""

    client = "android"

    def __init__(self, driver):
        self._own = AndroidCredStore(driver=driver)

    def read_map(self) -> dict[str, str]:
        """The account-store rows' ``account -> value`` map with the prefix
        stripped — :meth:`AppleForeignAccountStoreCredStore.read_map`'s shape."""
        prefix = ACCOUNT_STORE_FOREIGN_PREFIX
        return {k[len(prefix) :]: v for k, v in self._own.read_map().items() if k.startswith(prefix)}

    def stored_accounts(self) -> set[str]:
        return set(self.read_map())


def _attached_libsecret_store(driver, namespace: str) -> "LibsecretCredStore":
    """A ``LibsecretCredStore`` on the bus an already-launched linux
    ``use_real_keyring`` driver was handed — the ONE way both attach paths
    reach the launch's private Secret Service. The driver refused to launch
    without that bus, so its absence here is a harness bug, not a mode."""
    bus = (getattr(driver, "_launch_config", None) or {}).get("environment", {}).get(
        "DBUS_SESSION_BUS_ADDRESS"
    )
    if not bus:
        raise RuntimeError(
            "linux launched in use_real_keyring mode with no caller-owned bus "
            "recorded in its launch config — nothing to attach to"
        )
    return LibsecretCredStore(
        namespace, getattr(driver, "_resolved_xdg_base", ""), bus_address=bus
    )


def attach_account_store(client: str, driver) -> CredStore:
    """Bind a :class:`CredStore` to the shared ``fauna-account-store``
    namespace an **already-launched** driver's app process resolved —
    :func:`attach_cred_store`'s twin for the *second* namespace
    (``long-term-store.md`` § Cleanup contract: the T10 writer key + each
    account's ``principal_bundle`` slots, swept alongside the app's own
    namespace since the 2026-09-01 hole-3 fix).

    **Distinct from the app's own namespace under e2e, and that is the whole
    point of this function existing.** Every driver's ``FAUNA_KEYRING_APP``
    override, applied verbatim to the app's own namespace, is derived to
    ``{override}-account-store`` for this one
    (``fauna_credential_store::apply_namespace_override`` — the fix that
    keeps the two from collapsing onto one physical store under the harness,
    the way they used to). apple sets no override at all, so there the
    account-store namespace was — and remains — distinct by construction; see
    the two apple classes above for exactly how each already keeps it so.

    ``web`` has no second namespace: its account runtime's slot rests in the
    same ``localStorage`` as the registry, so the attach is the page's own
    store (:meth:`WebCredStore.read_map`). ``android``'s rides the foreign seam
    into its on-device credential file, read over the bridge
    (:class:`AndroidAccountStoreCredStore`)."""
    if client == "web":
        return WebCredStore(driver)
    if client == "android":
        return AndroidAccountStoreCredStore(driver)

    cred_dir = getattr(driver, "_resolved_credential_dir", None)
    if client == "macos":
        if not cred_dir:
            raise RuntimeError(f"driver for {client!r} recorded no `_resolved_credential_dir`")
        return AppleAccountStoreCredStore(cred_dir)
    if client == "ios":
        if not cred_dir:
            raise RuntimeError(f"driver for {client!r} recorded no `_resolved_credential_dir`")
        return AppleForeignAccountStoreCredStore(cred_dir)

    keyring_app = getattr(driver, "_resolved_keyring_app", None)
    if not keyring_app:
        raise RuntimeError(
            f"driver for {client!r} recorded no `_resolved_keyring_app` — either it "
            "has not launched yet, or it does not implement the cross-driver "
            "resolved-store contract (drivers/linux.py, tui.py, windows.py all set "
            "the pair in `launch()`)"
        )
    account_ns = account_store_namespace(keyring_app)

    if cred_dir is None:
        # linux `use_real_keyring`: the account-store namespace rides the same
        # private Secret Service the launch was handed, keyed by its own derived
        # namespace — attached to that bus, never a daemon of its own (a fresh
        # daemon would read an empty namespace and fail the precondition that
        # sign-in minted the writer key; measured 2026-09-15).
        if client != "linux":
            raise RuntimeError(
                f"{client!r} launched with no credential dir and has no keyring "
                "backend to fall back to"
            )
        return _attached_libsecret_store(driver, account_ns)

    if client in ("linux", "tui", "windows"):
        return FileCredStore(account_ns, cred_dir, getattr(driver, "_resolved_xdg_base", ""))
    raise ValueError(f"no account-store adapter for client {client!r}")


def make_file_backed_cred_store(client: str, tmp_path) -> CredStore:
    """Build the FILE-backend adapter for ``client``, even when that client's
    ``make_cred_store`` default uses a different backend (linux's real-keyring
    ``LibsecretCredStore``). Used to prove a specific case is headless-capable
    without changing every case's backend."""
    ns = f"fauna-launch-smoke-{uuid.uuid4().hex[:8]}"
    xdg_base = str(tmp_path / "xdg")
    if client == "linux":
        return LinuxFileCredStore(ns, str(tmp_path / "creds"), xdg_base)
    if client == "tui":
        return FileCredStore(ns, str(tmp_path / "creds"), xdg_base)
    if client in ("macos", "ios"):
        # Already file-backed by default (make_cred_store above) — no second
        # backend to force onto.
        return AppleFileCredStore(client, str(tmp_path / "creds"))
    if client == "windows":
        # Already file-backed by default (make_cred_store above) — same as apple.
        return WindowsFileCredStore(ns, str(tmp_path / "creds"), str(tmp_path / "data"))
    raise ValueError(f"no file-backed credential adapter for client {client!r}")


def requires_secret_service(client: str) -> bool:
    """True when ``client``'s adapter runs a Secret Service daemon of its own
    (``LibsecretCredStore``), which the box must be able to supply
    (``common.keyring.secret_service_available``). Only the libsecret-backed
    one does; the file backends need nothing."""
    return client == "linux"
