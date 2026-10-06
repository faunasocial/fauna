"""E2E test: verify the web build serves correctly under /app."""

import os
import subprocess
import time
import urllib.request

import pytest

from drivers.port_util import popen_group_kwargs, reap_descendants_of

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="session")
def web_build_dir():
    """Return the path to the built Svelte SPA, building if needed."""
    from common import get_repo_root
    repo = get_repo_root()
    build_dir = repo / "apps" / "fauna-web" / "build"
    if not (build_dir / "index.html").exists():
        pytest.skip("Web app not built (run 'just web' first)")
    # Ensure the /app symlink exists
    app_link = build_dir / "app"
    if not app_link.exists():
        os.symlink(".", str(app_link))
    return str(build_dir)


def test_web_app_serves_under_app_path(web_build_dir):
    """Start a temporary static server and verify /app/ returns 200 with HTML."""
    import socket
    # Find a free port
    with socket.socket() as s:
        s.bind(('', 0))
        port = s.getsockname()[1]

    proc = subprocess.Popen(
        ["python3", "-m", "http.server", str(port),
         "--bind", "127.0.0.1", "--directory", web_build_dir],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    try:
        # Wait for server to start
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                resp = urllib.request.urlopen(f"http://127.0.0.1:{port}/app/")
                break
            except Exception:
                time.sleep(0.2)
        else:
            pytest.fail("Static server did not start in 5s")

        assert resp.status == 200
        body = resp.read().decode()
        assert "<html" in body.lower()
        assert "fauna" in body.lower() or "_app/" in body
    finally:
        proc.kill()
        proc.wait()
