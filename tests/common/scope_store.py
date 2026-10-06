"""Per-app account-scoped state-DIRECTORY adapters — `cred_store.py`'s twin
for the ON-DISK half of `docs/goal/architecture/apps/account-scoping.md`
§ Erasure follows scope.

`cred_store.py`'s `attach_cred_store`/`attach_account_store` read specific
KNOWN files in the credential/account-store namespaces (a JSON map, or
libsecret rows). Neither reads the app's own account-scoped state
directory — the tree `mls_state.db` and friends live under
(`<flat-base>/<actor-id-hex>/`) — nor the sibling unified
`StoreRoot::platform()` root the same actor scopes under
(`account-scoping.md`'s ⚠: that root is "a
sibling of an app's flat base, not a child of it", so an erase iterating
only the app's own base silently misses it). This module answers "which
top-level base directories does THIS launch's sign-out erase sweep a
`<actor-id-hex>/` subdir under" so one cross-app test can assert the whole
directory tree is gone, not just the credential files inside it.

Mirrors `cred_store.py`'s shape: an `attach_scope_store(client, driver)`
discovers the adapter from an **already-launched** driver (the shared
`app`/`logged_in_app` fixture), exactly as `attach_cred_store` does — no
test knows which per-app subclass is underneath.
"""

from __future__ import annotations

from pathlib import Path


class ScopeStore:
    """Every top-level base directory THIS launch's account-scoped erase
    sweeps a `<actor-id-hex>/` subdirectory under
    (`fauna_account_store::db::erase_actor_state`'s `bases` argument, or the
    per-platform native twin — `AccountStateDir.erase`/`eraseAll` on apple,
    `AccountStateDir.Erase`/`EraseAll` on windows)."""

    #: Client this store belongs to.
    client: str

    def scope_roots(self) -> list[Path]:
        """The base directories, in no particular order."""
        raise NotImplementedError

    def scope_dirs(self, actor_id_hex: str) -> list[Path]:
        """The per-actor scoped directory under each base — what the erase
        removes wholesale (`std::fs::remove_dir_all`), one per root."""
        actor = actor_id_hex.strip().lower()
        return [root / actor for root in self.scope_roots()]


class XdgScopeStore(ScopeStore):
    """tui/linux: a flat base directly under `XDG_CONFIG_HOME`
    (`fauna-tui`/`fauna`), its own `backup/` subdir, and the shared unified
    sync root `<XDG_CONFIG_HOME>/fauna/sync` — both apps' `StoreRoot::platform()`
    resolves the SAME path (both driver's `config_home` isolates
    `XDG_CONFIG_HOME` per-launch, `drivers/tui.py`/`linux.py`), so this one
    class covers both (`apps/fauna-tui/src/account_scope.rs::erase_under`,
    `apps/fauna-linux/src/account_scope.rs::erase_under` — same three-base
    shape, only the flat name differs).

    ⚠ **`StoreRoot::platform()` is XDG-derived only on unix.** tui also runs on
    windows, where shared Rust's `production_base()` reads `LOCALAPPDATA` and
    ignores XDG entirely — so there the third root is
    `<LOCALAPPDATA>\\Fauna\\sync`, not `<config_home>/fauna/sync`, and the
    driver relocates `LOCALAPPDATA` and publishes the resolved path
    (`_resolved_store_root`) rather than letting this class re-spell the
    derivation. `store_root=None` keeps the unix derivation."""

    def __init__(
        self,
        client: str,
        config_home: str,
        flat_base_name: str,
        store_root: str | None = None,
    ):
        self.client = client
        self._config_home = Path(config_home)
        self._flat_base_name = flat_base_name
        self._store_root = Path(store_root) if store_root else None

    def scope_roots(self) -> list[Path]:
        flat = self._config_home / self._flat_base_name
        store = self._store_root or self._config_home / "fauna" / "sync"
        return [flat, flat / "backup", store]


class AppleScopeStore(ScopeStore):
    """macOS/iOS: `<app_support_dir>/Fauna` (the flat base,
    `AccountStateDir.base`) and its `sync` sibling — the unified store
    root, which on macOS resolves via the SAME relocated `HOME` (`storeContainerDir` is
    `nil` there, so shared Rust falls through to `StoreRoot::platform()`
    under the isolated home) and on iOS is `SyncStateDir.appSupportSyncDir`
    (`<app_support_dir>/Fauna/sync`, verbatim the same path as macOS's —
    both under the ONE isolated `Application Support` dir
    `drivers/macos.py`/`ios.py`'s `app_support_dir()` exposes)."""

    def __init__(self, client: str, app_support_dir: str):
        self.client = client
        self._app_support = Path(app_support_dir)

    def scope_roots(self) -> list[Path]:
        flat = self._app_support / "Fauna"
        return [flat, flat / "sync"]


class WindowsScopeStore(ScopeStore):
    """windows: `_resolved_data_dir` (`AccountStateDir.FlatBase`) and the
    unified store root — the same two-root shape as `AppleScopeStore`.

    `AccountStateDir.cs`'s `Erase`/`EraseAll` pass `storeContainer: null` (the
    desktop posture macOS also takes), so shared Rust resolves the second root
    through `StoreRoot::platform()` =
    `%LOCALAPPDATA%\\Fauna\\sync`. That made the root un-isolated for as long as
    `drivers/windows.py` did not relocate `LOCALAPPDATA`, so this class checked
    only the flat base; the driver now relocates it (e2e convention 10's third
    windows axis) and publishes the resolved root as `_resolved_store_root`,
    which is what this reads. The root is published rather than derived from
    `data_dir` because a CALLER-supplied `data_dir` is honoured verbatim and is
    not re-parented under the relocated `%LOCALAPPDATA%`, so `<data dir>/sync`
    is the right answer only for the default."""

    client = "windows"

    def __init__(self, data_dir: str, store_root: str):
        self._data_dir = Path(data_dir)
        self._store_root = Path(store_root)

    def scope_roots(self) -> list[Path]:
        return [self._data_dir, self._store_root]


def attach_scope_store(client: str, driver) -> ScopeStore:
    """Bind a :class:`ScopeStore` to the account-scoped directories an
    **already-launched** driver's app process resolved — the on-disk
    counterpart of `cred_store.py`'s `attach_cred_store`.

    Raises for `web` (no filesystem at all — browser `localStorage`) and
    `android` (the account-scoped directories are on-device, in the app's
    own `filesDir`; no host-side path exists to read them back — unlike the
    credential file, which the bridge's `GET /credentials` reads, no bridge
    route lists a directory tree yet)."""
    if client == "web":
        raise NotImplementedError(
            "web has no on-disk account-scoped directory at all — its state is "
            "browser localStorage, not a filesystem tree "
            "(long-term-store.md § Implementation status today)"
        )
    if client == "android":
        raise NotImplementedError(
            "android's account-scoped directories are on-device, in the app's own "
            "filesDir — no host-side path exists to read them back yet (the "
            "bridge's GET /credentials reads the credential file only)"
        )
    if client == "tui":
        # `_resolved_store_root` is set (windows) or None (unix) by the driver's
        # own launch — never re-derived here.
        return XdgScopeStore(
            client,
            driver.config_home,
            "fauna-tui",
            getattr(driver, "_resolved_store_root", None),
        )
    if client == "linux":
        return XdgScopeStore(client, driver.config_home, "fauna")
    if client in ("macos", "ios"):
        app_support = getattr(driver, "app_support_dir", None)
        base = app_support() if callable(app_support) else None
        if not base:
            raise RuntimeError(
                f"driver for {client!r} recorded no app_support_dir — either it has "
                "not launched yet, or launch() failed to relocate the store "
                "(drivers/macos.py / ios.py set it in launch())"
            )
        return AppleScopeStore(client, base)
    if client == "windows":
        data_dir = getattr(driver, "_resolved_data_dir", None)
        store_root = getattr(driver, "_resolved_store_root", None)
        if not data_dir or not store_root:
            raise RuntimeError(
                f"driver for {client!r} recorded no `_resolved_data_dir` / "
                "`_resolved_store_root` — either it has not launched yet, or "
                "launch() failed to relocate %LOCALAPPDATA% (drivers/windows.py "
                "sets both in launch())"
            )
        return WindowsScopeStore(data_dir, store_root)
    raise ValueError(f"no scope-dir adapter for client {client!r}")
