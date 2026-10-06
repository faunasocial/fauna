"""Cross-app launch/relaunch harness for the launch-routing e2e cases.

The launch-routing and nest-identity-pin cases all hinge on one lifecycle the
default e2e drivers don't expose directly: **boot a client with a persisted
identity, then force-quit and relaunch it with that identity still in place.**
Two app families realize that lifecycle differently, and this module hides
the difference behind one interface so a single test drives both (priority #1:
no per-app test divergence):

* **Native** (linux, tui): the identity is injected into an *external*
  persistent backend (libsecret / a file dir) BEFORE the binary boots, and a
  "relaunch" is a genuine ``teardown()`` + ``launch()`` — a fresh process that
  reads that backend back. The stable namespace lives in a ``CredStore``
  (``tests/common/cred_store.py``); ``preserve_state_across_relaunch()`` pins the
  driver's per-launch dirs to that namespace so the relaunch reuses them.
* **Web**: there is no external backend and no ``app_path``; the identity lives
  in the browser's ``localStorage`` on the SPA origin, seeded on the
  already-launched driver, and a "relaunch" is a ``hard_reload()`` — the one
  restart that keeps localStorage (a ``teardown()`` would discard the whole
  browser profile, identity included).
* **Android** is a native variant with no host-side store: the identity rides
  the launch itself (the bridge writes it on-device), and a relaunch is a
  force-stop + start that does not re-send it (``AndroidLaunchHarness``).

Both end a ``launch()`` with a running client that has run its launch path with
the given identity — wherever the routing table lands it (Online / a wizard page
/ the retry surface) — so the caller asserts the landing uniformly.

Two consumers now share it (priority #1 — no per-app launch-routing test
divergence): ``tests/test_nest_identity_pin.py`` and
``tests/test_onboarding_launch_routing_smoke.py``. The smoke module drives its
seed-and-launch routing cases through ``launch_and_route`` (web joins the cases
whose flow fits — B/C/D/E/F) and its force-quit-survival cases through
``launch()`` + ``relaunch()`` directly.
"""

from __future__ import annotations

import contextlib
import time

#: The authenticated main-app shell tab (``ui.yaml`` § feed).
FEED_TAB = "feed-tab"

#: The two shells that REPLACE the main sidebar **in place** while they show, each
#: named by its own ratified "leave the shell" affordance: `settings.md`
#: § Navigation model ("entering Settings swaps the normal nav for the settings
#: pages in the *same* sidebar slot — no second rail") and `admin.md`
#: § Navigation model, the shell it mirrors. So an app sitting in either shell is
#: authenticated and in the app with **no** ``feed-tab`` rendered anywhere — by
#: design, on every desktop app, not as a web quirk.
SHELL_SWAP_ANCHORS = ("settings-nav-back", "admin-nav-back")

#: Poll interval for the web "I'm in" wait. Latency-independent (convention 14):
#: a green run returns on the first tick and only a genuine failure spends the
#: caller's budget.
_POLL_INTERVAL_S = 0.3


def reached_authenticated_app(driver, timeout: float = 30) -> None:
    """Wait until the client is in the authenticated main app, using each app's
    proven "I'm in" signal: native flips ``session.authenticated``; web waits on a
    rendered shell, because its ``session.authenticated`` is not readable (see
    below). A driver-capability branch, sanctioned by the e2e conventions — shared
    by both launch-routing modules (identity-pin + the smoke join) so the signal is
    defined once.

    **Web waits on ANY of the three authenticated shells, not on ``feed-tab``
    alone.** The main sidebar that carries ``feed-tab`` is swapped out in place
    whenever the Settings or admin shell shows (``SHELL_SWAP_ANCHORS``), so a
    relaunch that comes back into one of them is fully authenticated with no
    ``feed-tab`` to wait for — and the old single-element wait then burned its
    whole budget and failed as a bare timeout, far from the cause. That is exactly
    what ``test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch``
    hit on web (2026-08-05): it sets the mode on Settings → Privacy, so
    ``recover()``'s ``location.reload()`` re-entered the *Settings* shell. It was
    filed as "web's injected login cannot survive a relaunch"; the identity had in
    fact survived intact (localStorage keeps ``fauna_secret``/``fauna_registered``
    and the SPA re-hydrates from them) — only the signal was shell-specific.

    Why web reads the DOM rather than the state flag the other six use: web's
    ``session.authenticated`` answers a **stricter** question than this helper
    asks. Until 2026-08-17 it answered none at all — the identity object
    ``store.ts::loadIdentity`` rebuilds from ``localStorage`` carried no
    ``registered`` field, so the flag came back ``undefined`` and
    ``JSON.stringify`` dropped the key. It now means *this identity was verified
    against its nest on this load* (the silent challenge came back verified),
    which is the live-session signal ``helpers.waiting.await_session_actor``
    needs. This helper wants something weaker — "the authenticated main shell is
    up" — and on web that is legitimately reachable from cache alone with the
    nest unreachable, so the shell wait stays the right signal here. Don't
    "unify" the two: swapping this to the flag would make every offline-shell
    relaunch time out."""
    if driver.is_web():
        anchors = (FEED_TAB, *SHELL_SWAP_ANCHORS)
        deadline = time.monotonic() + timeout
        while True:
            visible = next((a for a in anchors if driver.is_visible(a)), None)
            if visible is not None:
                return
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"web never reached an authenticated shell within {timeout}s: "
                    f"none of {list(anchors)} rendered. The app is either still "
                    "booting, or it booted signed out and is sitting on "
                    "onboarding — read the page URL and the console ring "
                    "(driver.console_log()) to tell those apart."
                )
            time.sleep(_POLL_INTERVAL_S)
    else:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=timeout
        )


class LaunchHarness:
    """Boot a client with a persisted identity, then force-quit + relaunch it,
    keeping that identity. Subclasses realize the native vs web lifecycle."""

    #: The client this harness drives ("linux" / "tui" / "web").
    client: str
    #: The live driver, populated by ``launch()``.
    driver = None

    def launch(self, *, secret_hex: str | None = None, node_url: str, trust: dict | None):
        """Start the client pointed at ``node_url`` and return the live driver.

        When ``secret_hex`` is given, persist that identity (+ ``node_url``)
        first, so the launch path runs the silent-challenge routing table against
        a stored identity (the seed-and-launch cases). Pass ``secret_hex=None`` to
        boot a FRESH client with no stored identity — the fresh-wizard cases that
        build their identity interactively (import a key, submit an invite) after
        boot; their persisted state still survives ``relaunch()`` because it lands
        in the same stable namespace this harness pins.

        ``trust`` is the harness nest answering at ``node_url`` — directly or
        behind a proxy — whose identity the launch's escrow-trust seed names
        (`e2e-automation-surface-gating.md` § The e2e trust seed), or ``None``
        when no nest answers there (a closed port, a host that never resolves)
        or the one that answers refuses every kind — a degraded nest's
        ``fauna.nest.outdated`` refuses ``nest.info`` itself, so the seed cannot
        read the identity it would name, and such a launch never gets past its
        silent challenge to an account runtime. It has no default, so every
        launch says which. web reads no environment and ignores it.

        Does NOT assume a particular landing — the caller asserts where the
        routing table (or the interactive flow) put it."""
        raise NotImplementedError

    def launch_and_route(self, *, secret_hex: str, node_url: str, trust: dict | None):
        """Boot with ``secret_hex`` AND run the launch-routing path to completion,
        so the client settles on whatever surface the routing table lands it —
        the caller then asserts that surface directly, with no relaunch of its own.

        This hides the one native/web asymmetry the seed-and-launch routing cases
        hit: a native binary runs its launch machine on boot, so ``launch()``
        already settles on the routed surface; the web SPA ``launch()`` only
        *seeds* localStorage (the reset surface is still up — the seam
        ``test_nest_identity_pin.py`` needs to set a pin before the first
        challenge), so web must reload once to run the routed path. Web overrides
        this; the native default is a plain ``launch()``. Use ``launch()`` +
        ``relaunch()`` directly for the force-quit-survival / interactive cases
        that must act between the two boots."""
        self.launch(secret_hex=secret_hex, node_url=node_url, trust=trust)
        return self.driver

    def relaunch(self) -> None:
        """Force-quit + relaunch, identity preserved (native: fresh process reads
        the backend; web: ``hard_reload()`` keeps localStorage)."""
        raise NotImplementedError

    def teardown(self) -> None:
        """Tear the client down and sweep any external namespace. Safe if never
        launched, and safe to call twice."""
        raise NotImplementedError


class NativeLaunchHarness(LaunchHarness):
    """linux / tui / macos / ios: inject the identity into a ``CredStore``
    backend before the binary boots; relaunch is a real ``teardown()`` +
    ``launch()``."""

    def __init__(
        self,
        client: str,
        cred_store,
        app_path: str,
        *,
        seed_trust,
        extra_launch_config: dict | None = None,
    ):
        self.client = client
        self._store = cred_store
        self._app_path = app_path
        self.driver = None
        self._config = None
        # The escrow-trust seed writer, `seed_trust(environment, nest)`, handed in
        # (`conftest._trust_seeder(request)`) because a shared module does not
        # import conftest; and the nest the current launch seeds for.
        self._seed_trust = seed_trust
        self._trust = None
        # Launch-config facts that are not about persisted identity, merged into
        # every `launch_config()` output rather than growing the CredStore
        # contract every other backend would then have to carry and ignore:
        # iOS's `udid` (drivers/ios.py `launch()` requires it alongside
        # `app_path`, and no store carries a simulator identity), and the
        # tray-residency case's `{"args": "--autostart"}`, whose whole subject is
        # HOW the binary was invoked rather than what was stored before it.
        self._extra_launch_config = extra_launch_config or {}

    @property
    def store(self):
        """The persistent credential store backing this harness. Exposed for the
        cases that introspect it directly — the factory-reset case reads
        ``store.stored_accounts()`` after a reset to prove the namespace is empty
        (a web driver has no such backend, which is one reason that case is
        native-only)."""
        return self._store

    @property
    def account_store(self):
        """The shared ``fauna-account-store`` namespace for this harness's LIVE
        driver — ``store``'s twin for the second namespace ``long-term-store.md``
        § Cleanup contract hole 3 widened the erase to reach.

        Unlike ``store`` (minted up front and handed to the driver via
        ``launch_config()``), this is resolved from the driver's own recorded
        ``_resolved_keyring_app`` / ``_resolved_credential_dir`` AFTER launch —
        the same way ``attach_account_store`` reads it for the ``logged_in_app``
        sign-out case — since this harness never mints the account-store
        namespace itself. Raises before ``launch()`` (the driver has recorded
        nothing yet)."""
        from common.cred_store import attach_account_store

        if self.driver is None:
            raise RuntimeError("account_store read before launch() — no live driver yet")
        return attach_account_store(self.client, self.driver)

    def launch(
        self,
        *,
        secret_hex: str | None = None,
        node_url: str,
        trust: dict | None,
        raw_fields: dict[str, str] | None = None,
    ):
        """``raw_fields`` are written verbatim after the identity
        (``CredStore.inject_raw``) — for a store the app did not write itself."""
        from drivers import create_driver

        self._store.clear()
        if secret_hex is not None:
            self._store.inject_identity(secret_hex=secret_hex, node_url=node_url)
        if raw_fields:
            self._store.inject_raw(raw_fields)
        self.driver = create_driver(self.client)
        # `launch()` stashes this same dict as the driver's `_launch_config`, so a
        # later `preserve_state_across_relaunch()` (which mutates it in place to
        # pin the stable dirs) reaches the config `relaunch()` re-launches with.
        store_config = self._store.launch_config(self._app_path, node_url)
        self._config = {**store_config, **self._extra_launch_config}
        # `environment` merges one level down rather than replacing: linux's
        # real-keyring store hands its private bus in through this key, and a
        # case's own env (`@pytest.mark.launch_env`, the wrong-clock seed) must
        # ride alongside it, not evict it.
        if "environment" in store_config or "environment" in self._extra_launch_config:
            self._config["environment"] = {
                **store_config.get("environment", {}),
                **self._extra_launch_config.get("environment", {}),
            }
        # Keyed by the url the app dials, read from the nest's own port — the
        # two differ only behind a proxy, where the app's authority is the proxy's.
        self._trust = None if trust is None else {"url": node_url, "port": trust["port"]}
        self._seed(self._config)
        self.driver.launch(self._config)
        return self.driver

    def relaunch(self) -> None:
        self.driver.teardown()
        self._seed(self._config)
        self.driver.launch(self._config)

    def _seed(self, config: dict) -> None:
        """Seed ``config``'s escrow trust in this launch's nest, read afresh on
        every launch: a relaunch after a rotation must name the identity the nest
        serves now, not the one it served at the first boot."""
        if self._trust is not None:
            self._seed_trust(config.setdefault("environment", {}), self._trust)

    def teardown(self) -> None:
        if self.driver is not None:
            with contextlib.suppress(Exception):
                self.driver.teardown()
        with contextlib.suppress(Exception):
            self._store.clear()
        # After the sweep: linux's store runs its own Secret Service daemon.
        with contextlib.suppress(Exception):
            self._store.close()


class AndroidLaunchHarness(NativeLaunchHarness):
    """android: the identity is sent with the launch, not written before it
    (``AndroidCredStore`` — the bridge writes it into the app's own ``filesDir``
    on the ``/session`` POST), and a relaunch is ``teardown()`` + ``launch()``
    WITHOUT that seed: the bridge force-stops the app, and the file the app has
    been writing to since is what the new process reads back. Re-sending the
    launch's seed would overwrite it, and a survival case would pass on the
    harness's copy of the identity rather than the app's.

    A case's launch env (``@pytest.mark.launch_env``) crosses only if the bridge
    carries the name: an intent launch has no process environment, so every
    name must be read by ``BridgeHttpServer``'s ``/session`` handler, put on the
    intent, and re-exported by ``MainActivity``'s ``Os.setenv`` door. Anything
    else is REFUSED, as on web — silently dropping a seed would let the case
    pass on a launch that never saw it. (The run-level names conftest seeds —
    the trust seed, the real-conversations gate — reach the bridge by the same
    route and are not a case's to choose, so they are not listed here.)"""

    #: Case-level launch-env names the android bridge carries into the process.
    #: ``FAUNA_E2E_CLOCK_OFFSET_SECS``: the launch clock's e2e offset
    #: (``fauna_launch_machine::launch_clock::OFFSET_ENV``), case L's seed.
    #: Pinned against the Kotlin by ``tests/test_android_launch_harness.py``.
    ANDROID_LAUNCH_ENV = frozenset({"FAUNA_E2E_CLOCK_OFFSET_SECS"})

    def __init__(self, cred_store, app_path: str, *, seed_trust, extra_launch_config=None):
        unknown = set((extra_launch_config or {}).get("environment") or {}) - self.ANDROID_LAUNCH_ENV
        if unknown:
            raise ValueError(
                f"android launch harness cannot honor launch env {sorted(unknown)}: "
                "an intent launch has no process environment, and only "
                f"{sorted(self.ANDROID_LAUNCH_ENV)} are carried by the bridge into "
                "MainActivity's Os.setenv door — add the name there first"
            )
        super().__init__(
            "android", cred_store, app_path,
            seed_trust=seed_trust, extra_launch_config=extra_launch_config,
        )

    def relaunch(self) -> None:
        # Once launched, the device file is the store; the seed was a one-shot.
        self._config.pop("seed_credentials", None)
        super().relaunch()


class WebLaunchHarness(LaunchHarness):
    """web: the identity lives in the SPA origin's ``localStorage``; there is no
    binary and no ``app_path``. ``launch()`` starts a fresh browser, seeds the
    identity on it, then ``hard_reload()``s so the SPA launch path runs with the
    identity. ``relaunch()`` is another ``hard_reload()`` — the only restart that
    keeps localStorage.

    ``environment`` is a case's launch env (``@pytest.mark.launch_env``). A
    browser has no process environment, so only the names in
    :data:`WEB_LAUNCH_ENV` are honored — each is written into localStorage under
    its own name beside the identity, where the SPA's test flavor reads it
    before launch — and any other name is REFUSED: silently dropping a seed
    would let the case pass on a launch that never saw it."""

    client = "web"

    #: Launch-env names the web test flavor reads back out of localStorage.
    #: ``FAUNA_E2E_CLOCK_OFFSET_SECS``: the launch clock's e2e offset, read by
    #: ``libs/fauna-wasm-launch``'s ``LaunchMachine`` constructor (the native
    #: apps read the same name from their environment,
    #: ``fauna_launch_machine::launch_clock::OFFSET_ENV``).
    WEB_LAUNCH_ENV = frozenset({"FAUNA_E2E_CLOCK_OFFSET_SECS"})

    def __init__(self, spa_url: str, environment: dict[str, str] | None = None):
        #: The SPA proxy origin the wasm pins by (== the web ``node_url``).
        self._spa_url = spa_url
        unknown = set(environment or {}) - self.WEB_LAUNCH_ENV
        if unknown:
            raise ValueError(
                f"web launch harness cannot honor launch env {sorted(unknown)}: a "
                "browser has no process environment, and only "
                f"{sorted(self.WEB_LAUNCH_ENV)} have a localStorage twin the SPA "
                "reads — add one (and its reader) before running this case on web"
            )
        self._environment = dict(environment or {})
        self.driver = None
        self._cred = None

    def launch(
        self,
        *,
        secret_hex: str | None = None,
        node_url: str,
        trust: dict | None,
        raw_fields: dict[str, str] | None = None,
    ):
        """``raw_fields`` are written verbatim after the identity
        (``WebCredStore.inject_raw``) — for a store the app did not write
        itself, e.g. an unparseable ``fauna/index`` blob. ``trust`` is ignored: a
        browser reads no environment, the trust seed's declared absence."""
        from common.cred_store import WebCredStore
        from drivers import create_driver

        self.driver = create_driver("web")
        # Launch at the `/app/` route + settle, exactly as the session driver cache
        # does (`conftest._build_app_config` → `spa + "/app/"`, then
        # `wait_for_state`). The BARE origin `/` redirects (SvelteKit `goto`) on
        # load with an empty store, and that navigation races the driver's own
        # agent injection ("Execution context was destroyed"). The pin key is still
        # the ORIGIN (`node_url` == `self._spa_url`), which the browser reaches on
        # the same origin — only the launch *path* differs.
        self.driver.launch({"url": self._spa_url.rstrip("/") + "/app/"})
        with contextlib.suppress(Exception):
            self.driver.wait_for_state(lambda s: s is not None, timeout=30)
        self._cred = WebCredStore(self.driver)
        # Seed the identity into localStorage but do NOT reload here: the SPA stays
        # UNAUTHENTICATED on the reset launch surface. The caller seeds the pin next
        # and then `relaunch()` — that `hard_reload()` is the FIRST run of the
        # pinned silent-challenge launch path. Authenticating first (a stray reload
        # that logs the identity in) would cache session state that bypasses the
        # pinned challenge on the reload, so the identity-changed surface would
        # never render — the exact regression that hides this whole behavior.
        #
        # ``secret_hex=None`` boots a fresh SPA with an empty store (the interactive
        # cases that import a key after launch); the localStorage the interactive
        # flow then writes still survives ``relaunch()``'s ``hard_reload()``.
        self._cred.clear()
        if secret_hex is not None:
            self._cred.inject_identity(secret_hex=secret_hex, node_url=node_url)
        if raw_fields:
            self._cred.inject_raw(raw_fields)
        if self._environment:
            # After `clear()`, beside the identity: the launch-env twins the SPA's
            # test flavor reads before `start()` (see WEB_LAUNCH_ENV). They survive
            # `relaunch()`'s `hard_reload()` with the identity.
            self._cred.inject_raw({k: str(v) for k, v in self._environment.items()})
        return self.driver

    def launch_and_route(
        self,
        *,
        secret_hex: str,
        node_url: str,
        trust: dict | None,
        raw_fields: dict[str, str] | None = None,
    ):
        # `launch()` only seeds localStorage (leaving the SPA on the reset
        # surface); the reload is the FIRST run of the routed launch path with the
        # seeded identity — the browser twin of a native binary routing on boot.
        self.launch(secret_hex=secret_hex, node_url=node_url, trust=trust, raw_fields=raw_fields)
        self.relaunch()
        return self.driver

    def relaunch(self) -> None:
        self.driver.hard_reload()

    def teardown(self) -> None:
        if self.driver is not None:
            with contextlib.suppress(Exception):
                self.driver.teardown()


def make_launch_harness(
    client: str, *, tmp_path=None, app_path=None, spa_url=None, file_backed=False,
    udid=None, extra_launch_config=None, seed_trust=None,
) -> LaunchHarness:
    """Build the launch harness for ``client``.

    * web needs ``spa_url`` (the SPA proxy origin the browser targets and pins by).
    * native (linux/tui/macos/ios/windows) needs ``tmp_path`` (roots the
      ``CredStore`` namespace) + ``app_path`` (the binary to launch) +
      ``seed_trust`` (``conftest._trust_seeder(request)``), the writer that puts
      the escrow-trust seed for each launch's ``trust`` nest into its
      environment. Without it every launch would run a dormant generation
      plane, silently, so a native harness refuses to be built without one.
    * ios ALSO needs ``udid`` (the already-booted simulator ``drivers/ios.py``
      ``launch()`` requires alongside ``app_path`` — pass ``ios_setup["udid"]``).

    ``extra_launch_config`` is merged into every ``launch_config()`` this harness
    hands the driver, for the cases whose subject is *how the binary was invoked*
    rather than what was stored before it — the tray-residency case passes
    ``{"args": "--autostart"}``. It goes here rather than into the ``CredStore``
    contract on purpose: a store describes persisted identity, and every backend
    would have to carry and ignore a field about command lines (the same
    reasoning that put ios's ``udid`` on this seam).

    ``file_backed`` forces the native store onto the shared file backend
    (``FAUNA_E2E_CREDENTIAL_DIR``) even for linux, whose default is the real
    session Secret Service. The pending-invite case wants this: its slot lives on
    the shared ``AccountRegistry``, which honors the file backend independently of
    an unlocked keyring — so that case is headless-safe (ignored for web, whose
    store is the browser's localStorage regardless)."""
    if client == "web":
        if spa_url is None:
            raise ValueError("web launch harness needs spa_url (the SPA proxy origin)")
        extra = extra_launch_config or {}
        unsupported = sorted(set(extra) - {"environment"})
        if unsupported:
            raise ValueError(
                f"web launch harness cannot honor launch config {unsupported}: "
                "the SPA launches no binary"
            )
        return WebLaunchHarness(spa_url, environment=extra.get("environment"))
    if tmp_path is None or app_path is None:
        raise ValueError(f"native launch harness ({client!r}) needs tmp_path + app_path")
    if seed_trust is None:
        raise ValueError(
            f"native launch harness ({client!r}) needs seed_trust "
            "(conftest._trust_seeder(request)), or its launches run with an empty "
            "escrow trust set"
        )
    if client == "ios" and udid is None:
        raise ValueError("native launch harness ('ios') needs udid (see ios_setup)")
    from common.cred_store import make_cred_store, make_file_backed_cred_store

    if client == "android":
        from pathlib import Path

        from helpers import android_device

        # `tests/common/` → the repo root the APK paths are relative to.
        repo_root = Path(__file__).resolve().parents[2]
        extra = {**android_device.run_launch_facts(repo_root), **(extra_launch_config or {})}
        return AndroidLaunchHarness(
            make_cred_store("android", tmp_path), app_path,
            seed_trust=seed_trust, extra_launch_config=extra,
        )
    store = (
        make_file_backed_cred_store(client, tmp_path)
        if file_backed
        else make_cred_store(client, tmp_path)
    )
    extra = {**({"udid": udid} if client == "ios" else {}), **(extra_launch_config or {})}
    return NativeLaunchHarness(
        client, store, app_path, seed_trust=seed_trust, extra_launch_config=extra or None
    )
