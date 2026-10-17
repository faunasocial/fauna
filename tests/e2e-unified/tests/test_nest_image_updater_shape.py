"""tier_1: the nest image carries what the Fauna updater needs — static
properties of the checked-in Dockerfile and its two build drivers
(``nest/update-channel.md`` § The decision, § The updater).

* **The version label.** The decision compares the
  ``org.opencontainers.image.version`` label of the running image and of the
  channel's head, read without a pull; the Dockerfile stamps it from the
  ``FAUNA_VERSION`` build arg, which both build drivers — the image workflow
  and ``just docker-push-dev`` — must pass, as they pass ``FAUNA_BUILD_COMMIT``.
  An image built without it carries an empty label, which the decision reads
  as version zero.
* **The updater binary.** ``fauna-updater`` ships beside ``fauna-nest`` at
  ``/usr/local/bin/fauna-updater`` (the bundles' entrypoint override), built in
  a ``cargo build`` invocation of its own: a dependency only the updater needs
  (the Docker client today, a signature verifier later) must never
  feature-unify into the nest's build.

Same reasoning as ``test_nest_image_revision_label.py``: a static property of
the tree needs no image build.
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_REPO_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
)


def _read(rel: str) -> str:
    with open(os.path.join(_REPO_ROOT, rel), encoding="utf-8") as fh:
        return fh.read()


def test_the_image_carries_a_version_label_from_the_version_build_arg():
    text = _read("Dockerfile")
    assert re.search(r"^ARG FAUNA_VERSION=", text, re.MULTILINE)
    assert re.search(
        r"^\s*org\.opencontainers\.image\.version=\$\{FAUNA_VERSION\}",
        text,
        re.MULTILINE,
    ), "the final stage must stamp org.opencontainers.image.version=${FAUNA_VERSION}"


def test_every_justfile_image_build_passes_the_version():
    text = _read("justfile")
    commit = len(re.findall(r"--build-arg FAUNA_BUILD_COMMIT=", text))
    version = len(re.findall(r"--build-arg FAUNA_VERSION=", text))
    assert commit > 0 and version == commit, (
        f"justfile: {commit} image builds pass FAUNA_BUILD_COMMIT, {version} FAUNA_VERSION"
    )


def test_both_workflow_legs_pass_the_version():
    """The self-hosted leg's shared args and the hosted leg's final image."""
    text = _read(".github/workflows/build-nest-image.yml")
    assert "--build-arg FAUNA_VERSION=${{ steps.version.outputs.version }}" in text
    assert re.search(
        r"^\s+FAUNA_VERSION=\$\{\{ steps\.version\.outputs\.version \}\}$",
        text,
        re.MULTILINE,
    )


def test_the_updater_is_built_alone_and_ships_beside_the_nest():
    text = _read("Dockerfile")
    builds = re.findall(r"cargo build --locked --release ([^\\\n&]*)", text)
    updater = [b for b in builds if "fauna-updater" in b]
    assert len(updater) == 1, f"one invocation builds fauna-updater: {builds}"
    assert re.fullmatch(r"-p fauna-updater\s*", updater[0]), (
        f"fauna-updater must be the only package of its invocation: {updater[0]!r}"
    )
    assert re.search(
        r"^COPY --from=rust-native /out/fauna-updater /usr/local/bin/fauna-updater$",
        text,
        re.MULTILINE,
    )
