"""tier_1: the s6 service graph stops the nest while the SNI router still splices.

On `docker stop` s6-rc brings services down in reverse dependency order: a
service's dependents stop before it does. On a deployed box every client reaches
the nest through `fauna-sni-router`'s `:443` (the nest binds container loopback),
so the order of those two decides whether the nest's graceful-shutdown WS 1001
reaches anyone. With the router a dependent of the nest (the shape before
2026-10-20), s6-rc stopped the router first — cutting every live splice — and only
then sent the nest its SIGTERM, so every redeploy dropped every app's socket with
no close frame (`test_docker_stop_emits_ws_1001` read a bare TCP drop).

The order is a static property of `docker/s6/*/dependencies.d`, so it is pinned
here with no image build; the tier_4 `tests/platform/docker/test_graceful_shutdown.py`
proves the 1001 actually reaches the wire through the real image.

See docs/goal/architecture/installers/docker.md § s6-overlay Services (the
shutdown order) and docs/goal/architecture/transport-connection.md § Graceful
shutdown.
"""

import os

import pytest

pytestmark = pytest.mark.tier_1

_REPO_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
)
_S6_ROOT = os.path.join(_REPO_ROOT, "docker", "s6")

NEST = "fauna-nest"
ROUTER = "fauna-sni-router"


def _dependencies(service):
    d = os.path.join(_S6_ROOT, service, "dependencies.d")
    return set(os.listdir(d)) if os.path.isdir(d) else set()


def _depends_on(service, target, seen=None):
    """True when `service` transitively depends on `target` (only services with a
    source directory here are followed; `base` and the other s6-overlay
    built-ins have none)."""
    seen = set() if seen is None else seen
    for dep in _dependencies(service):
        if dep == target:
            return True
        if dep not in seen and os.path.isdir(os.path.join(_S6_ROOT, dep)):
            seen.add(dep)
            if _depends_on(dep, target, seen):
                return True
    return False


def test_the_nest_depends_on_the_router():
    """The nest stops first, while the router still splices its 1001 out."""
    assert _depends_on(NEST, ROUTER), (
        f"docker/s6/{NEST} must depend on {ROUTER} (a marker "
        f"docker/s6/{NEST}/dependencies.d/{ROUTER}) so s6-rc stops the nest "
        f"BEFORE the router at `docker stop`: the nest's WS 1001 drain travels "
        f"through the router's live :443 splices. Found {NEST} dependencies: "
        f"{sorted(_dependencies(NEST))}"
    )


def test_the_router_does_not_depend_on_the_nest():
    """The inverse edge would make the router stop first (and a cycle with the
    one above, which s6-rc refuses to compile)."""
    assert not _depends_on(ROUTER, NEST), (
        f"docker/s6/{ROUTER} depends on {NEST}: s6-rc then stops the router "
        f"first at `docker stop`, cutting every live :443 splice before the "
        f"nest can send its WS 1001. Found {ROUTER} dependencies: "
        f"{sorted(_dependencies(ROUTER))}"
    )
