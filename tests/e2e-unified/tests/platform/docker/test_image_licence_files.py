"""E2E test (tier_4): the nest image carries its licences, and serves the web app's.

shipped-notices.md § Per channel, the Nest image and Web app rows. The staging
is pinned statically by ``tests/test_shipped_notices_nest_web.py`` (tier_1);
this module reads the built artifact, which is the only place a stage that
silently wrote nothing, a COPY to the wrong directory or a label a later
instruction dropped can show.

Two cheap halves: one ``docker run --rm --entrypoint /bin/sh`` and one
``docker inspect`` (no nest boot), then a booted container's ``/app`` for the
web app's three files. The generated notices files are dependency-authored
text, so nothing here prints or searches their bodies: presence, size and the
first line — the generator's own header wording — are what is read
(shipped-notices.md § How the notices file is produced). The two licence texts
are the repository's own files and are compared byte for byte.
"""

import json
import ssl
import subprocess
import urllib.request
from pathlib import Path

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

_DOC_DIR = "/usr/share/doc/fauna"
_SPA_DIR = "/usr/share/fauna-web"


@pytest.fixture(scope="module")
def docker_image():
    return docker_build(get_repo_root())


def _sh(tag: str, script: str) -> str:
    proc = subprocess.run(
        ["docker", "run", "--rm", "--entrypoint", "/bin/sh", tag, "-c", script],
        capture_output=True, text=True, timeout=300,
    )
    assert proc.returncode == 0, f"the container failed ({proc.returncode}): {proc.stderr[-400:]}"
    return proc.stdout


def _file(tag: str, path: str) -> bytes:
    proc = subprocess.run(
        ["docker", "run", "--rm", "--entrypoint", "/bin/cat", tag, path],
        capture_output=True, timeout=300,
    )
    assert proc.returncode == 0, f"{path} is not in the image: {proc.stderr[-400:]!r}"
    return proc.stdout


def _first_line_and_size(tag: str, path: str) -> tuple[str, int]:
    out = _sh(tag, f'test -s {path} && head -n 1 {path} && wc -c < {path}').splitlines()
    return out[0], int(out[1])


def test_the_image_carries_the_two_licence_texts_byte_for_byte(docker_image):
    repo = Path(get_repo_root())
    for name in ("LICENSE-MIT", "LICENSE-APACHE"):
        assert _file(docker_image, f"{_DOC_DIR}/{name}") == (repo / name).read_bytes(), (
            f"{_DOC_DIR}/{name} is not the repository's {name}"
        )


def test_the_image_carries_its_generated_notices(docker_image):
    first, size = _first_line_and_size(docker_image, f"{_DOC_DIR}/THIRD-PARTY-NOTICES")
    assert first.startswith("Third-party notices for Fauna nest "), (
        "the image's notices file does not open with the generator's header for the nest-image artifact"
    )
    assert size > 100_000, f"the image's notices file is {size} bytes — far too small to cover the image's closure"


def test_the_image_labels_its_licences(docker_image):
    proc = subprocess.run(
        ["docker", "inspect", "--format", "{{json .Config.Labels}}", docker_image],
        capture_output=True, text=True, timeout=60,
    )
    assert proc.returncode == 0, proc.stderr[-400:]
    labels = json.loads(proc.stdout) or {}
    assert labels.get("org.opencontainers.image.licenses") == "MIT OR Apache-2.0", labels


def test_the_served_tree_holds_the_web_apps_three_files(docker_image):
    repo = Path(get_repo_root())
    for name in ("LICENSE-MIT", "LICENSE-APACHE"):
        assert _file(docker_image, f"{_SPA_DIR}/{name}.txt") == (repo / name).read_bytes()
    first, _ = _first_line_and_size(docker_image, f"{_SPA_DIR}/THIRD-PARTY-NOTICES.txt")
    assert first.startswith("Third-party notices for Fauna for the web "), (
        "the served tree's notices file does not open with the generator's header for the web-app artifact"
    )


@pytest.fixture()
def nest(docker_image):
    port = find_free_port()
    name = f"fauna-nest-licences-{port}"
    start_container(name, port)
    try:
        wait_for_health(port, name)
        yield port
    finally:
        remove_container(name)


def test_the_nest_serves_the_web_apps_licence_files_beside_its_entry_page(nest):
    repo = Path(get_repo_root())
    ctx = ssl._create_unverified_context()
    for name, expected in (
        ("LICENSE-MIT.txt", (repo / "LICENSE-MIT").read_bytes()),
        ("LICENSE-APACHE.txt", (repo / "LICENSE-APACHE").read_bytes()),
        ("THIRD-PARTY-NOTICES.txt", None),
    ):
        resp = urllib.request.urlopen(f"https://127.0.0.1:{nest}/app/{name}", timeout=15, context=ctx)
        body = resp.read()
        assert resp.status == 200, f"GET /app/{name} returned {resp.status}"
        assert resp.headers.get("Content-Type", "").startswith("text/plain"), (
            f"/app/{name} is served as {resp.headers.get('Content-Type')!r}, not text"
        )
        if expected is not None:
            assert body == expected, f"/app/{name} is not the repository's licence text"
        else:
            assert body.startswith(b"Third-party notices for Fauna for the web "), (
                "/app/THIRD-PARTY-NOTICES.txt is not the web app's generated notices file"
            )
