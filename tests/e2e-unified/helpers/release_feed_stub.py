"""A stub of the release feed the apps' newer-version check reads.

`installers/README.md` § Knowing a newer version is out: on a platform where you
install the app yourself, you can ask the app whether a newer version is out,
and when one is, it says so and where to get it. Every app that checks reads
ONE endpoint — `fauna_core::version::latest_release_api_url`, the GitHub
Releases API's `releases/latest` for `RELEASE_REPO` — and folds the answer
through the shared `newer_release_from_latest_json`.

A walk of that promise cannot read the real feed: it needs the network, GitHub
rate-limits anonymous reads, and "what is newest today" is not a fixture. So the
harness serves the endpoint itself and hands the app its origin through the
compile-gated `FAUNA_E2E_RELEASE_FEED_URL` seam (tui: `settings/about.rs`,
`feed_origin`; convention 15 — the release binary neither reads nor names it).
Only the ORIGIN is stubbed: the path the app requests, the JSON shape it parses
and the semver decision are the production ones, which is what makes the walk a
witness of the promise rather than of the stub.

**What it advertises.** By default the running version (the workspace's own,
`running_tag()`), so the once-per-sign-in look every tui sign-in now makes
(§ Knowing a newer version is out, amended 2026-10-03) finds nothing newer and
paints nothing on any other test's Settings landing. A walk that needs "a newer
version is out" says so with `advertising(NEWER_TAG)` — a tag deliberately far
above any version the tree will ever carry, so it reads "newer" on every commit
without chasing the workspace version. `newer_release_from_latest_json` strips
the `v`.

`requests` records a path only once its answer has been written, so a walk
that has seen the look's request knows the app holds the answer.
"""

from __future__ import annotations

import contextlib
import json
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from common import get_repo_root

#: The repository every checker names (`fauna_core::version::RELEASE_REPO`).
RELEASE_REPO = "faunasocial/fauna"

#: The tag the stub feed advertises. Far above any real version on purpose.
NEWER_TAG = "v99.0.0"

#: The one path the production check requests (`latest_release_api_url`).
LATEST_PATH = f"/repos/{RELEASE_REPO}/releases/latest"


def workspace_version() -> str:
    """The one product version, read from the root `Cargo.toml`'s
    `[workspace.package]` — the same line `mac-dmg-package` and the version
    gates read."""
    text = (Path(get_repo_root()) / "Cargo.toml").read_text()
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    assert match, "root Cargo.toml carries no `version = \"…\"` line"
    return match.group(1)


def running_tag() -> str:
    """The running version, spelled as the feed spells a tag."""
    return f"v{workspace_version()}"


class ReleaseFeedStub:
    """Serves ``GET /repos/<RELEASE_REPO>/releases/latest`` on a loopback port."""

    def __init__(self, tag: str | None = None):
        self.tag = tag or running_tag()
        self.requests: list[str] = []
        stub = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):  # silence the default stderr log
                pass

            def do_GET(self):  # noqa: N802 (BaseHTTPRequestHandler's spelling)
                if self.path != LATEST_PATH:
                    self.send_response(404)
                    self.end_headers()
                    stub.requests.append(self.path)
                    return
                body = json.dumps(
                    {
                        "tag_name": stub.tag,
                        "html_url": f"https://github.com/{RELEASE_REPO}/releases/tag/{stub.tag}",
                    }
                ).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                self.wfile.flush()
                stub.requests.append(self.path)

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(
            target=self._server.serve_forever, name="release-feed-stub", daemon=True
        )

    @property
    def origin(self) -> str:
        """What the app is handed as its feed origin (no trailing slash)."""
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    @contextlib.contextmanager
    def advertising(self, tag: str):
        """Advertise `tag` for the block's duration, then the previous tag again
        — the stub is session-wide, so a walk never leaves its answer behind."""
        previous, self.tag = self.tag, tag
        try:
            yield self
        finally:
            self.tag = previous

    def start(self) -> "ReleaseFeedStub":
        self._thread.start()
        return self

    def stop(self) -> None:
        self._server.shutdown()
        self._server.server_close()
