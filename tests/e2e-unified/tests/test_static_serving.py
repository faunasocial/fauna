"""Smoke test: verify the nest serves SPA static files with correct MIME types.

If static_dir is misconfigured or the SPA build is stale, JS module scripts
get served as text/html (the SPA fallback), causing the entire app to fail.
This test catches that before any browser-based tests run.
"""
import urllib.request

import pytest


pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

# This test only makes sense for the web app (SPA static files).
import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from conftest import get_available_apps
if "web" not in get_available_apps():
    pytest.skip("web client not selected", allow_module_level=True)


@pytest.mark.feature("nest-serves-the-app")
def test_spa_js_modules_have_correct_content_type(nest_instance, static_dir):
    """JS files referenced by index.html must be served as JavaScript, not HTML."""
    import re
    from pathlib import Path

    nest_url = nest_instance["url"]

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
