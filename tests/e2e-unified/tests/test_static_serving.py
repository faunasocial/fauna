"""Smoke test: verify the nest serves SPA static files with correct MIME types.

If static_dir is misconfigured or the SPA build is stale, JS module scripts
get served as text/html (the SPA fallback), causing the entire app to fail.
This test catches that before any browser-based tests run.
"""
import urllib.request

import pytest


# `web`: the SPA is this test's subject, so it belongs to a web run. The marker
# is also what lets `_prebuild_web_spa` see the run needs the SPA and build it
# at collection — without it the whole `just web-test` build landed inside this
# test's 900 s budget and timed out in setup.
pytestmark = [pytest.mark.tier1, pytest.mark.tier_3, pytest.mark.web]

# This test only makes sense for the web app (SPA static files).
import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from conftest import get_available_apps
if "web" not in get_available_apps():
    pytest.skip("web client not selected", allow_module_level=True)


@pytest.fixture()
def spa_serving_nest(request, nest_mode, tmp_path_factory, static_dir):
    """A nest booted with the SPA build (``--static-dir``), as the deployment
    artifact boots it. The session ``nest_instance`` serves no build — web tests
    reach the app through ``_serve_spa_proxy`` — so asking it for ``/app/…``
    measures nothing but a 404."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "spa-serving-nest", static_dir=static_dir
    )
    yield nest
    cleanup()


@pytest.mark.feature("nest-serves-the-app")
def test_spa_js_modules_have_correct_content_type(spa_serving_nest, static_dir):
    """JS files referenced by index.html must be served as JavaScript, not HTML."""
    import re
    from pathlib import Path

    nest_url = spa_serving_nest["url"]

    # Read index.html and extract JS module paths
    index_html = (Path(static_dir) / "index.html").read_text()
    js_paths = re.findall(r'(?:src|href)="(/app/_app/immutable/[^"]+\.js)"', index_html)
    assert js_paths, "index.html should reference at least one JS module"

    # Check the first JS module has a JS content type (not text/html from fallback)
    resp = urllib.request.urlopen(f"{nest_url}{js_paths[0]}")
    content_type = resp.headers.get("Content-Type", "")
    assert "javascript" in content_type or "application/wasm" in content_type, (
        f"Expected JS content type for {js_paths[0]}, got '{content_type}'. "
        "This usually means the SPA build is stale — run 'just web' to rebuild."
    )


@pytest.mark.feature("nest-serves-the-app")
def test_missing_asset_under_the_hashed_tree_is_not_found_not_the_page(spa_serving_nest):
    """A stale content-hashed asset URL (a tab opened before a redeploy) must be
    404 — never the app's page with 200, which the browser would try to run as
    a script. A deep link still gets the page: the app routes it in the browser."""
    import urllib.error

    nest_url = spa_serving_nest["url"]

    with pytest.raises(urllib.error.HTTPError) as miss:
        urllib.request.urlopen(f"{nest_url}/app/_app/immutable/does-not-exist.js")
    assert miss.value.code == 404
    assert miss.value.headers.get("X-Frame-Options") == "DENY", (
        "the mount's security headers ride the 404 too"
    )
    assert b"<html" not in miss.value.read().lower(), "a miss must not carry the app's page"

    deep = urllib.request.urlopen(f"{nest_url}/app/feed/some/deep/route")
    assert deep.status == 200
    assert b"<html" in deep.read().lower()
