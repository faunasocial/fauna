"""tier_1: the upgrade smoke's answer when the pinned previous build is gone.

`scripts/image-upgrade-smoke.sh` boots the pinned previous build
(`tests/e2e-unified/pinned-previous-build.toml`) and then the candidate on the
same `/data`. A pin naming an image the registry no longer holds — every nest
package was reset on 2026-10-11, so the pin named a deleted build — has no
previous release to upgrade from: the smoke says so and passes, and the next
verified build re-records the pin (image-pipeline.md § Image tags & channels).
Any OTHER registry error is not "no previous build" and fails the smoke, so
an outage or a refused credential never reads as a clean first run.

The public pipeline passes `--previous-image <its :latest>` in place of the pin
(the two-pipelines design part 4): the ref is resolved to its digest and that
digest is what boots, with no pin-ancestry window; an anonymous caller refused
a token — GHCR's answer for a package that is absent or not public — is "no
previous release" on that path alone, since every box pulls `:latest`
anonymously.

A stub `docker` on PATH stands in for the registry; nothing is pulled or run.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_SMOKE = _REPO / "scripts" / "image-upgrade-smoke.sh"

# `STUB_INSPECT` picks the registry's answer to `imagetools inspect`:
# `missing` → the not-found text a registry gives for a deleted tag;
# `denied`  → an unrelated failure (auth refused);
# `anon`    → GHCR refusing an anonymous token (absent or non-public package);
# anything else → the image exists, at the digest printed.
_STUB_DOCKER = """#!/bin/sh
echo "$*" >> "$STUB_LOG"
case "$*" in
    *"imagetools inspect"*)
        case "$STUB_INSPECT" in
            missing) echo "ERROR: ghcr.io/faunasocial/nest:sha-0: not found" >&2; exit 1 ;;
            denied) echo "ERROR: unexpected status: 403 Forbidden" >&2; exit 1 ;;
            anon) echo "ERROR: failed to authorize: failed to fetch anonymous token: unexpected status from GET request to https://ghcr.io/token?scope=repository%3Afaunasocial%2Fnest%3Apull&service=ghcr.io: 403 Forbidden" >&2; exit 1 ;;
        esac
        echo "sha256:0123abcd"
        ;;
    *"pull"*|*"run"*)
        # The smoke must never get this far in these cases.
        echo "stub: unexpected $1" >&2
        exit 9
        ;;
esac
exit 0
"""


def _run(
    tmp_path: Path, inspect: str, previous_image: str | None = None
) -> tuple[subprocess.CompletedProcess[str], str]:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    stub = bin_dir / "docker"
    stub.write_text(_STUB_DOCKER)
    stub.chmod(0o755)
    log = tmp_path / "docker.log"
    log.touch()
    pin = tmp_path / "pin.toml"
    pin.write_text(
        'commit = "0000000000000000000000000000000000000000"\n'
        'nest_image = "ghcr.io/faunasocial/nest:sha-0"\n'
    )
    env = dict(os.environ)
    env["PATH"] = f"{bin_dir}{os.pathsep}{env['PATH']}"
    env["STUB_LOG"] = str(log)
    env["STUB_INSPECT"] = inspect
    source = ["--previous-image", previous_image] if previous_image else ["--pin-file", str(pin)]
    proc = subprocess.run(
        ["bash", str(_SMOKE), "ghcr.io/faunasocial/nest:sha-1-amd64", *source],
        capture_output=True,
        text=True,
        env=env,
        cwd=_REPO,
        timeout=120,
    )
    return proc, log.read_text()


def test_a_pinned_build_the_registry_no_longer_holds_is_no_previous_build(tmp_path):
    proc, docker_log = _run(tmp_path, "missing")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "no previous build" in proc.stdout, proc.stdout
    assert "pull" not in docker_log and "run " not in docker_log, docker_log


def test_any_other_registry_error_fails_the_smoke(tmp_path):
    proc, docker_log = _run(tmp_path, "denied")
    assert proc.returncode == 1, proc.stdout + proc.stderr
    assert "403 Forbidden" in proc.stderr, proc.stderr
    assert "pull" not in docker_log, docker_log


def test_the_pin_path_keeps_an_anonymous_refusal_a_failure(tmp_path):
    proc, docker_log = _run(tmp_path, "anon")
    assert proc.returncode == 1, proc.stdout + proc.stderr
    assert "pull" not in docker_log, docker_log


@pytest.mark.parametrize("inspect", ["missing", "anon"])
def test_a_public_latest_no_box_can_pull_is_no_previous_release(tmp_path, inspect):
    proc, docker_log = _run(tmp_path, inspect, "ghcr.io/faunasocial/nest:latest")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "SKIP" in proc.stdout, proc.stdout
    assert "pull" not in docker_log and "run " not in docker_log, docker_log


def test_the_previous_image_boots_by_the_digest_it_resolved_to(tmp_path):
    """The stub refuses the pull, so the run stops there — after naming the
    exact manifest it would boot."""
    proc, docker_log = _run(tmp_path, "exists", "ghcr.io/faunasocial/nest:latest")
    assert "is ghcr.io/faunasocial/nest@sha256:0123abcd" in proc.stdout, proc.stdout
    assert "pull --platform linux/amd64 ghcr.io/faunasocial/nest@sha256:0123abcd" in docker_log, docker_log
    assert "expected-incompatible" not in proc.stdout + proc.stderr
