"""E2E test (tier_4): convention 15's *artifact* clause, performed rather than asserted.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention —
convention 15 says the automation surface is "absent from the release/production
artifact, **verifiable by a `strings`/grep of the built artifact**". For web that
clause was backed by a verification *event* (a build inspected on 2026-07-24),
not by a test — and it decayed: on 2026-09-02 a grep of the SPA inside
`ghcr.io/faunasocial/nest:latest` returned `__fauna_message_banner_mount_count`,
installed bare from a component's `onMount` in every build.

`tests/test_web_automation_surface_gating.py` (tier_1) now holds the **source**
side of that gap. It cannot hold this side: a pin that reads the tree says
nothing about what a build step, a dependency, or a stale published image
actually emitted. So this module does the grep the convention names, inside the
real artifact, and it is the only place in the suite that can.

Cheap by construction — one `docker run --rm --entrypoint /bin/sh`, no nest boot,
no claim, no browser. The SPA the image serves lives at `/usr/share/fauna-web`
(`Dockerfile`'s `COPY --from=web-builder … /usr/share/fauna-web/`; the entrypoint
reconciles `[nest].static_dir` to it on every boot).
"""

import subprocess

import pytest

from .helpers import docker_build, get_repo_root

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

# No app marker: no driver, no browser — this reads a file tree inside an image.
pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

#: Where the image serves the SPA from.
_SPA_DIR = "/usr/share/fauna-web"

#: The wasm-side e2e setters. `conftest.py::static_dir` states the contract these
#: witness: `just web-test` builds `fauna-wasm-onboarding` with `test-helpers` and
#: ships them; "production builds use `just web` and don't ship those setters".
_TEST_SETTERS = (
    "setHandleCheckSnapshotForTest",
    "setStepForTest",
    "setInviteRequestSnapshotForTest",
)


@pytest.fixture(scope="module")
def docker_image():
    """The shipped image, reused — never built here (see ``docker_build``)."""
    return docker_build(get_repo_root())


def _grep_spa(tag: str, pattern: str) -> list[str]:
    """Files under the served SPA matching `pattern`, or []."""
    proc = subprocess.run(
        [
            "docker", "run", "--rm", "--entrypoint", "/bin/sh", tag,
            "-c", f'grep -rl "{pattern}" {_SPA_DIR} 2>/dev/null || true',
        ],
        capture_output=True, text=True, timeout=300,
    )
    assert proc.returncode == 0, (
        f"the grep container itself failed ({proc.returncode}): {proc.stderr[-400:]}"
    )
    return [ln for ln in proc.stdout.splitlines() if ln.strip()]


def test_the_wasm_test_setters_are_absent_from_the_shipped_spa(docker_image):
    """The production SPA carries none of the onboarding machine's e2e setters.

    This is the half that is green today, and it is worth its own assertion
    rather than being folded into the one below: it is the *reason*
    `test_onboarding_localhost.py` and `test_onboarding_handle_check_reset.py`
    are PREMISE on the nest-mode axis (`test_nest_mode_axis.py`) — they drive the
    machine over a bridge the shipped bundle does not expose. If this ever goes
    red, those two dispositions are wrong and the image is shipping a test build.
    """
    for setter in _TEST_SETTERS:
        hits = _grep_spa(docker_image, setter)
        assert not hits, (
            f"the shipped SPA carries the e2e setter {setter!r} in {hits} — the "
            f"image is serving a `just web-test` bundle, not a production one "
            f"(conftest.py::static_dir draws that line)"
        )


@pytest.mark.xfail(
    strict=True,
    reason=(
        "the published image predates the 2026-09-02 fix: "
        "`__fauna_message_banner_mount_count` was installed bare from "
        "MessageBanner.svelte's onMount in every build, and the tag on this "
        "machine still contains it. Delete this marker once an image built after "
        "that fix is the one being tested — the XPASS is the signal that the fix "
        "reached the artifact, which is the whole thing this module measures."
    ),
)
def test_no_fauna_automation_hook_reaches_the_shipped_spa(docker_image):
    """No `window.__fauna_*` name survives into the production bundle.

    The literal form of convention 15's own "verifiable by a `strings`/grep of the
    built artifact". A hit here means either a bare install in a component (the
    2026-09-02 leak, now source-pinned) or a gated installer that stopped being
    tree-shaken — and the second is invisible to any pin that reads the tree,
    which is why this module exists alongside the tier_1 one rather than instead
    of it.
    """
    hits = _grep_spa(docker_image, "__fauna_")
    assert not hits, (
        f"the shipped SPA carries a `window.__fauna_*` automation name in {hits}. "
        f"Grep the file for `__fauna_[A-Za-z0-9_]*` to see which; then find its "
        f"install site in `apps/fauna-web/src/` and put it behind "
        f"`__FAUNA_E2E_AUTOMATION__` or inside `$lib/e2e-automation`."
    )
