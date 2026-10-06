"""Shared fixtures for ordered scenario tests.

Each scenario module defines its own module-scoped `scenario` fixture
that creates a fresh nest and users. This conftest provides helpers
and the SPA server fixture that scenarios can use.
"""
import http.server
import os
import sys
import threading
import urllib.error
import urllib.request as urllib_req
from dataclasses import dataclass, field
from pathlib import Path

import pytest

_tests_dir = str(Path(__file__).resolve().parent.parent.parent.parent)
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)

_e2e_unified_dir = str(Path(__file__).resolve().parent.parent.parent)
if _e2e_unified_dir not in sys.path:
    sys.path.insert(0, _e2e_unified_dir)

from common import build_node, create_actor_and_register
from drivers import create_driver
from drivers.port_util import find_free_port
from actions import ActionLayer


@dataclass
class ScenarioState:
    """Shared state for an ordered scenario.

    nest/alice/bob are set at scenario start. Tests pass data forward
    via the mutable `shared` dict (e.g. scenario.shared["post_id"] = ...).
    """
    nest: dict
    alice: dict
    bob: dict
    shared: dict = field(default_factory=dict)

    @property
    def alice_secret_hex(self) -> str:
        return self.alice["signing_key"].encode().hex()

    @property
    def bob_secret_hex(self) -> str:
        return self.bob["signing_key"].encode().hex()


@pytest.fixture(scope="session")
def nest_binary():
    """Build fauna-nest once per session."""
    return build_node()


@pytest.fixture(scope="session")
def static_dir():
    """Path to the built Svelte SPA."""
    repo_root = Path(__file__).resolve().parent.parent.parent.parent.parent
    path = repo_root / "apps" / "fauna-web" / "build"
    if not path.exists():
        pytest.skip("Web app not built — run 'just web' first")
    return str(path)


def _resolve_linux_binary() -> Path | None:
    """Return the path to the fauna-linux debug binary, or None."""
    cargo_target = os.environ.get("CARGO_TARGET_DIR")
    if cargo_target:
        p = Path(cargo_target) / "debug" / "fauna-desktop"
        if p.exists():
            return p
    repo_root = Path(__file__).resolve().parent.parent.parent.parent.parent
    p = repo_root / "target" / "debug" / "fauna-desktop"
    if p.exists():
        return p
    return None


def create_scenario(request, nest_mode, tmp_path_factory, label: str):
    """Start a fresh nest with two registered users. Returns ``(state, cleanup)``.

    The nest comes from the run's mode provider rather than from a
    ``find_free_port`` + ``start_nest`` pair inlined here (``testing.md`` § Default
    app and nest mode, ruling (1)). This helper is a plain function called by two
    module-scoped fixtures, which is why neither AST pin could see the spawn: the
    binary arrived as the CALLER's fixture parameter and there was no nest-starting
    fixture to grade. It asks the provider for nothing, so it is the zero-option
    call in every mode.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, label)
    port = nest["port"]
    alice = create_actor_and_register(port, admin_signing_key=nest["admin"]["signing_key"])
    bob = create_actor_and_register(port, admin_signing_key=nest["admin"]["signing_key"])
    return ScenarioState(nest=nest, alice=alice, bob=bob), cleanup


def start_spa_server(static_dir: str, nest_url: str) -> tuple[str, http.server.HTTPServer]:
    """Start a SPA server proxying API calls to the given nest URL.

    Returns (spa_url, server). Caller must call server.shutdown().
    """

    class SPAHandler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=static_dir, **kwargs)

        def _is_api_path(self):
            return self.path.startswith("/api/") or self.path.startswith("/admin/")

        def do_GET(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            path = self.path
            if path.startswith("/app/"):
                path = path[4:]
            elif path == "/app":
                path = "/"
            file_path = Path(static_dir) / path.lstrip("/")
            if file_path.exists() and file_path.is_file():
                self.path = path
                super().do_GET()
            else:
                self.path = "/index.html"
                super().do_GET()

        def do_POST(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            self.send_error(405)

        def do_PUT(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            self.send_error(405)

        def do_DELETE(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            self.send_error(405)

        def do_PATCH(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            self.send_error(405)

        def do_OPTIONS(self):
            self.send_response(200)
            self.send_header("Access-Control-Allow-Origin", "*")
            self.send_header("Access-Control-Allow-Methods",
                             "GET, POST, PUT, DELETE, PATCH, OPTIONS")
            self.send_header("Access-Control-Allow-Headers",
                             "Authorization, Content-Type")
            self.end_headers()

        def _proxy_to_nest(self):
            target = nest_url + self.path
            length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(length) if length else None
            req = urllib_req.Request(
                target, data=body, method=self.command,
                headers={k: v for k, v in self.headers.items()
                         if k.lower() not in ("host",)},
            )
            try:
                resp = urllib_req.urlopen(req)
                self.send_response(resp.status)
                for k, v in resp.headers.items():
                    if k.lower() not in ("transfer-encoding",):
                        self.send_header(k, v)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()
                self.wfile.write(resp.read())
            except urllib.error.HTTPError as e:
                self.send_response(e.code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()
                self.wfile.write(e.read())
            except Exception as e:
                self.send_error(502, str(e))

        def log_message(self, format, *args):
            pass

    spa_port = find_free_port()
    server = http.server.HTTPServer(("127.0.0.1", spa_port), SPAHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return f"http://127.0.0.1:{spa_port}", server


def make_web_app(spa_url: str, nest_url: str, secret_hex: str,
                 actor_id_hex: str) -> ActionLayer:
    """Create a logged-in web ActionLayer using the bridge driver.

    Each call spawns a separate bridge process with its own browser.
    """
    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})

    import time
    time.sleep(1)

    layer = ActionLayer(driver)
    layer.auth.login(
        node_url=nest_url,
        username=actor_id_hex,
        password="",
        secret_hex=secret_hex,
    )
    return layer


def make_linux_app(nest: dict, secret_hex: str,
                   actor_id_hex: str, request) -> ActionLayer | None:
    """Create a logged-in Linux ActionLayer, or None if binary not found."""
    from conftest import _seeded_environment

    linux_bin = _resolve_linux_binary()
    if linux_bin is None:
        return None
    driver = create_driver("linux")
    driver.launch({
        "app_path": str(linux_bin),
        "environment": _seeded_environment(request, nest),
    })
    layer = ActionLayer(driver)
    layer.auth.login(
        node_url=nest["url"],
        username=actor_id_hex,
        password="",
        secret_hex=secret_hex,
    )
    return layer


def make_cached_app(driver_cache, app_name: str, nest_url: str,
                    secret_hex: str, actor_id_hex: str,
                    handle: str = "scenario-user",
                    spa_url: str | None = None,
                    nest: dict | None = None) -> ActionLayer:
    """Create a logged-in ActionLayer using the session-scoped driver cache.

    Reuses the existing cached driver for `app_name`, avoiding the
    resource conflicts caused by spawning separate bridge/app instances.
    Auth is injected via set_state() — no UI login needed.

    Pass `nest` (the fixture's raw dict, `nest_url` came from) so a cached
    driver from an earlier scenario — launched trusting a DIFFERENT nest — is
    relaunched trusting this one BEFORE the injected session points it there
    (`e2e-automation-surface-gating.md` § The e2e trust seed).
    """
    import time

    driver = driver_cache(app_name)
    if nest is not None:
        from conftest import _relaunch_trusting_nest

        _relaunch_trusting_nest(driver, nest)
    driver.reset()
    time.sleep(0.5)

    node = spa_url if app_name == "web" and spa_url else nest_url
    driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node,
            "secret_hex": secret_hex,
            "actor_id": actor_id_hex,
            "handle": handle,
            "device_id": f"test-device-scenario-{handle}",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    time.sleep(1)  # let auth + data load settle
    return ActionLayer(driver)
