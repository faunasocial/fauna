"""tier_4 e2e: the real nest image actually serves the bundled web SPA at ``/app``.

**The regression this exists for.** ``docker/entrypoint.sh``'s ``[nest]`` overlay
was a ``sed`` anchored on a ``require_registration`` line that
``config/default.toml`` stopped shipping (2026-07-13). A sed
address matching nothing is a silent no-op with exit 0, so every Docker nest
first-booted on or after that date got a ``nest.toml`` with no ``static_dir``;
``bins/fauna-nest/src/lib.rs`` then never calls ``mount_spa``, there is no
``/app`` route at all, and the request falls through to ``web_content_or_info``
→ the nest info page. ``https://<domain>/app`` served **byte-identical HTML to
``https://<domain>/``** for eleven days, found live on a real box 2026-07-24.

**Why tier_4 and not tier_3.** The bytes were in the image the whole time
(``Dockerfile``'s ``COPY --from=web-builder … /usr/share/fauna-web/``) and the
mount mechanism was correct and tested — ``bins/fauna-nest/tests/spa_security_headers.rs``
exercises ``mount_spa`` directly against a temp dir. What was broken was the
deployment artifact *configuring* it, which by definition only a real image under
real supervision can observe. Textbook mechanism-tested / wiring-untested gap.

The cheap half of the gate is ``tests/docker/test_entrypoint_overlay.py``
(``just entrypoint-test``, on the merge path): it proves the overlay writes the
key. This file proves the consequence end-to-end — that a container from the real
image answers ``/app`` with the SPA.

Run against a **published** image rather than a local build (local nest-image
builds are forbidden on dev VMs)::

    docker pull --platform linux/amd64 ghcr.io/faunasocial/nest:latest
    docker tag ghcr.io/faunasocial/nest:latest fauna-nest-test:local
    FAUNA_REUSE_IMAGE=1 pytest tests/e2e-unified/tests/platform/docker/test_spa_serving.py -v
"""

import html
import re
import ssl
import subprocess
import urllib.error
import urllib.request

import pytest

from .helpers import (
    docker_build,
    find_free_port,
    get_repo_root,
    remove_container,
    start_container,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
    pytest.mark.self_contained_docker,
]

# A marker unique to the BUILT SvelteKit SPA: `apps/fauna-web/src/app.html` hard-codes
# this manifest link, and `svelte.config.js` sets `paths.base = '/app'`. Deliberately
# not `<title>Fauna</title>` — the info page's `<title>Fauna Nest</title>` contains it
# as a prefix, so that assertion would pass on the bug.
SPA_MARKER = '/app/manifest.json'

# The info page's own heading (`lib.rs::info_page`). Its presence at /app IS the
# bug: a positive-only assert cannot catch this, because the info page is also
# 200 + text/html.
INFO_PAGE_MARKER = 'Fauna Nest API'


def _get(port: int, path: str):
    """GET over the box's self-signed floor; returns (status, body, headers)."""
    req = urllib.request.urlopen(
        f"https://127.0.0.1:{port}{path}", timeout=15,
        context=ssl._create_unverified_context(),
    )
    return req.status, req.read().decode("utf-8", "replace"), dict(req.headers)


@pytest.fixture(scope="module")
def docker_image():
    return docker_build(get_repo_root())


@pytest.fixture()
def nest(docker_image):
    port = find_free_port()
    name = f"fauna-nest-spa-{port}"
    start_container(name, port)
    try:
        wait_for_health(port, name)
        yield port, name
    finally:
        remove_container(name)


@pytest.mark.feature("nest-serves-the-app")
def test_app_path_serves_the_spa_not_the_info_page(nest):
    """The whole bug, in one assertion pair."""
    port, name = nest
    status, body, _ = _get(port, "/app/")

    assert status == 200, f"GET /app/ returned {status}"
    assert SPA_MARKER in body, (
        f"GET /app/ did not serve the web SPA.\n"
        f"static_dir is almost certainly missing from /data/nest.toml, so "
        f"mount_spa never ran and there is no /app route.\n"
        f"--- nest.toml ---\n{_nest_toml(name)}\n"
        f"--- first 600 bytes of body ---\n{body[:600]}"
    )
    assert INFO_PAGE_MARKER not in body, (
        f"GET /app/ served the nest INFO PAGE. This is the 2026-07-13 regression: "
        f"the info page is also 200 + text/html, which is why the positive assert "
        f"alone is not enough.\n--- first 600 bytes ---\n{body[:600]}"
    )


@pytest.mark.feature("nest-serves-the-app")
def test_app_and_root_are_not_the_same_page(nest):
    """The user-visible symptom was that `/` and `/app` returned byte-identical
    HTML. Assert the contrast directly — it is the sharpest statement of the bug
    and it stays meaningful even if both markers above are someday renamed."""
    port, _ = nest
    _, root_body, _ = _get(port, "/")
    _, app_body, _ = _get(port, "/app/")

    assert INFO_PAGE_MARKER in root_body, (
        "apex should still serve the info page when no web content is hosted "
        "(this test's premise, not the behaviour under test)"
    )
    assert root_body != app_body, (
        "/ and /app/ returned byte-identical HTML — the nest info page is "
        "answering at /app because the SPA was never mounted"
    )


@pytest.mark.feature("nest-serves-the-app")
def test_spa_origin_security_headers_ride_the_real_mount(nest):
    """`mount_spa` is also what emits the SPA-origin headers of
    `web-content-hosting.md` § Same-origin security model invariant #5. With
    `static_dir` unset the mount never happens, so the *headers* were missing on
    every affected box too — a security consequence of the same one-line defect
    that `spa_security_headers.rs` (which mounts directly) could never see."""
    port, _ = nest
    _, _, headers = _get(port, "/app/")
    lower = {k.lower(): v for k, v in headers.items()}

    assert lower.get("x-frame-options") == "DENY", lower
    assert "frame-ancestors 'none'" in lower.get("content-security-policy", ""), lower
    assert lower.get("x-content-type-options") == "nosniff", lower
    assert lower.get("referrer-policy") == "no-referrer", lower


def _get_raw(port: int, path: str):
    """GET → (status, body text, the raw header message), never raising on an
    error status: repeated headers (``Set-Cookie``) survive, and a 404's headers
    are as much under test as a 200's."""
    try:
        resp = urllib.request.urlopen(
            f"https://127.0.0.1:{port}{path}", timeout=15,
            context=ssl._create_unverified_context(),
        )
        return resp.status, resp.read().decode("utf-8", "replace"), resp.headers
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), e.headers


def _policies(headers, body: str) -> list[str]:
    """Every Content-Security-Policy the page is under: the response headers'
    and the document's own ``<meta http-equiv>`` (where SvelteKit's hash-mode
    resource policy lives)."""
    found = list(headers.get_all("Content-Security-Policy") or [])
    for tag in re.findall(r"<meta\b[^>]*>", body, flags=re.I):
        if re.search(r'http-equiv\s*=\s*["\']content-security-policy["\']', tag, re.I):
            m = re.search(r'content\s*=\s*"([^"]*)"', tag) or re.search(
                r"content\s*=\s*'([^']*)'", tag)
            if m:
                found.append(html.unescape(m.group(1)))
    return found


def _directive(policies: list[str], name: str) -> list[str] | None:
    """The source list of directive ``name`` in whichever policy states it."""
    for policy in policies:
        for part in policy.split(";"):
            tokens = part.split()
            if tokens and tokens[0].lower() == name:
                return tokens[1:]
    return None


def _script_srcs(body: str) -> list[str]:
    return re.findall(r'<script\b[^>]*\bsrc\s*=\s*["\']([^"\']+)["\']', body, flags=re.I)


@pytest.mark.feature("nest-serves-the-app")
def test_the_served_app_runs_only_its_own_scripts(nest):
    """The app page the nest serves is under a policy that lets it run only
    scripts from its own address — no inline script except the ones it names by
    hash, nothing from another site, no plugins (`web-content-hosting.md`
    § Implementation status today: the resource CSP `script-src 'self'
    'wasm-unsafe-eval'`, no `'unsafe-inline'`, `object-src 'none'`). Read from
    the page and headers the real image serves, which is what a browser obeys."""
    port, _ = nest
    status, body, headers = _get_raw(port, "/app/")
    assert status == 200 and SPA_MARKER in body, f"GET /app/ did not serve the SPA: {status}"
    policies = _policies(headers, body)

    script_src = _directive(policies, "script-src") or _directive(policies, "default-src")
    assert script_src is not None, f"the app page is under no script policy: {policies}"
    lowered = [t.lower() for t in script_src]
    assert "'self'" in lowered, f"script-src must admit the app's own scripts: {script_src}"
    assert "'unsafe-inline'" not in lowered, f"inline scripts are allowed: {script_src}"
    assert "'unsafe-eval'" not in lowered, f"string eval is allowed: {script_src}"
    outside = [t for t in lowered
               if t == "*" or t.endswith(":") or "://" in t or "." in t.split("'")[0]]
    assert not outside, f"script-src admits scripts from elsewhere: {outside} in {script_src}"

    object_src = _directive(policies, "object-src")
    assert object_src is not None and [t.lower() for t in object_src] == ["'none'"], (
        f"plugins must be refused (object-src 'none'): {object_src} in {policies}"
    )

    for src in _script_srcs(body):
        assert src.startswith("/app/") or not re.match(r"^[a-z][a-z0-9+.-]*:|^//", src, re.I), (
            f"the app page loads a script from elsewhere: {src!r}"
        )


@pytest.mark.feature("nest-serves-the-app")
def test_the_served_app_never_sets_a_cookie(nest):
    """The app the nest serves hands a browser no cookie — not on its page, its
    manifest, its scripts, or a miss (`web-content-hosting.md` invariant 2: no
    auth cookie may ever be introduced). The sites a nest serves are walked at
    tier_3 in `tests/api/test_web_served_headers.py`."""
    port, _ = nest
    status, body, _ = _get_raw(port, "/app/")
    assert status == 200 and SPA_MARKER in body, f"GET /app/ did not serve the SPA: {status}"
    paths = ["/app/", "/app", "/app/manifest.json", "/app/no-such-route",
             "/app/_app/no-such-asset.js",
             *[s for s in _script_srcs(body) if s.startswith("/app/")]]
    cookies = []
    for path in paths:
        st, _, hd = _get_raw(port, path)
        if hd.get_all("Set-Cookie"):
            cookies.append(f"{path} ({st}): {hd.get_all('Set-Cookie')}")
    assert not cookies, "the served app handed a browser a cookie:\n" + "\n".join(cookies)


@pytest.mark.feature("nest-serves-the-app")
def test_a_box_with_no_static_dir_self_heals_on_restart(nest):
    """A `nest.toml` with no `static_dir` (its first-run block never runs
    again) must be repaired: the overlay reconciles `static_dir` on EVERY boot.
    Reproduce that exact on-disk state and assert the box repairs itself.

    Without the every-boot reconcile this test fails and such a box stays broken
    permanently, which is precisely what makes first-run-only the wrong shape for
    an artifact constant.
    """
    port, name = nest
    # Sanity: the box is healthy and serving the SPA before we break it.
    assert SPA_MARKER in _get(port, "/app/")[1]

    # Reproduce the broken-window on-disk shape.
    _exec(name, ["sed", "-i", "/^static_dir/d", "/data/nest.toml"], user="fauna")
    assert "static_dir" not in _nest_toml(name), "failed to reproduce the broken state"

    subprocess.run(["docker", "restart", name], capture_output=True, timeout=90, check=True)
    wait_for_health(port, name)

    assert "static_dir" in _nest_toml(name), (
        f"restart did not restore static_dir — the overlay is first-run-only, so "
        f"every box deployed during the broken window stays broken after pulling "
        f"the fixed image.\n--- nest.toml ---\n{_nest_toml(name)}"
    )
    assert SPA_MARKER in _get(port, "/app/")[1], "SPA still not served after self-heal"


# ── helpers ───────────────────────────────────────────────────────────


def _exec(name: str, argv: list[str], user: str | None = None) -> str:
    cmd = ["docker", "exec"]
    if user:
        cmd += ["-u", user]
    cmd += [name, *argv]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    if proc.returncode != 0:
        raise RuntimeError(f"{' '.join(argv)} failed in {name}:\n{proc.stderr}")
    return proc.stdout


def _nest_toml(name: str) -> str:
    return _exec(name, ["cat", "/data/nest.toml"])
