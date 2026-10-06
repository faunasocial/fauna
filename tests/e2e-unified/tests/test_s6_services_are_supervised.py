"""tier_1: every s6 service in the image must be reachable from a bundle.

s6-rc only brings a service under live supervision if it is reachable from a
bundle's `contents.d`. A service directory can therefore be complete in every
other respect — `run`, `type`, `dependencies.d`, a dedicated UID, a Dockerfile
build+COPY step, an entry in `fauna-supervisor`'s allowlist — and still never
start, silently, in the shipped image.

That is exactly what happened to `fauna-atproto-bridge`: it had all of the
above and no `contents.d` marker, so an admin enabling ATProto/ATProto hosting
had no `/run/service/fauna-atproto-bridge` for the supervisor's `up` command to
act on. The bridge simply never started, with nothing in the product saying so.

tier_3 cannot catch this class — it drives the binaries directly and bypasses
`docker/s6/*` by design (the documented tier_3 blind spot). A tier_4 test under
real Docker supervision would catch it, but so does this: "is the service
reachable from the bundle" is a static property of the checked-in tree, and
answering it needs no image build, no container, and no network. Per the
split-the-mile rule, the mechanism is tested headlessly here; a tier_4 test
would additionally prove the service actually comes up.

See docs/goal/architecture/installers/docker.md, and
docs/goal/behavior/atproto-pds-bridge.md (S1 packaging).
"""

import os

import pytest

pytestmark = pytest.mark.tier_1

_REPO_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
)
_S6_ROOT = os.path.join(_REPO_ROOT, "docker", "s6")


def _service_dirs():
    """Every s6 source directory, split into (longruns, bundles)."""
    longruns, bundles = [], []
    for name in sorted(os.listdir(_S6_ROOT)):
        type_path = os.path.join(_S6_ROOT, name, "type")
        if not os.path.isfile(type_path):
            continue
        with open(type_path, encoding="utf-8") as fh:
            kind = fh.read().strip()
        (bundles if kind == "bundle" else longruns).append(name)
    return longruns, bundles


def _bundle_contents(bundle):
    d = os.path.join(_S6_ROOT, bundle, "contents.d")
    return set(os.listdir(d)) if os.path.isdir(d) else set()


def test_the_tree_has_longruns_and_at_least_one_bundle():
    """Guard the guard: if discovery silently found nothing, the real
    assertion below would pass vacuously."""
    longruns, bundles = _service_dirs()
    assert longruns, f"no longrun services discovered under {_S6_ROOT}"
    assert bundles, f"no bundle discovered under {_S6_ROOT}"


def test_every_longrun_service_is_in_a_bundle():
    """A longrun with no `contents.d` entry is dead weight in the image: built,
    shipped, and never supervised."""
    longruns, bundles = _service_dirs()
    supervised = set()
    for bundle in bundles:
        supervised |= _bundle_contents(bundle)

    orphans = sorted(set(longruns) - supervised)
    assert not orphans, (
        "s6 longrun service(s) not reachable from any bundle's contents.d, so "
        f"s6-rc will never supervise them: {orphans}. Add an empty marker file "
        f"docker/s6/<bundle>/contents.d/<service> (same convention as the "
        f"existing entries). Bundles checked: {bundles}"
    )


def test_no_bundle_names_a_service_that_does_not_exist():
    """The inverse drift: a marker left behind after a service was renamed or
    removed makes s6-rc fail to compile the whole bundle at image build."""
    longruns, bundles = _service_dirs()
    known = set(longruns) | set(bundles)
    for bundle in bundles:
        dangling = sorted(_bundle_contents(bundle) - known)
        assert not dangling, (
            f"docker/s6/{bundle}/contents.d names service(s) with no source "
            f"directory: {dangling}"
        )
