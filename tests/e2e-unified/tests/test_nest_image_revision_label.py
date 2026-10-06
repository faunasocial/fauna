"""tier_1: the nest image build always stamps an OCI revision label.

`docker inspect ghcr.io/faunasocial/nest:latest --format '{{json .Config.Labels}}'`
used to print `null` — nothing could programmatically answer "which ref was
this artifact built from" without pulling the image and reading a commit's
timestamp against it (testing.md class (7), the stale-tag hazard: a 15-day-old
tag turned 107 passed into 108 `fauna.auth.signature_failed` errors on
2026-08-29). The nest image already threads the built commit as an env var
(`FAUNA_BUILD_COMMIT`) for the release-candidate gate to
read off `.Config.Env`; this closes the other half by also stamping it as the
*standard* `org.opencontainers.image.revision` label, so a tool that reads
labels rather than env gets the same answer in one `docker inspect`.

Asserting this by building an image and inspecting it would make the test's
correctness depend on which image happens to be on the machine — the exact
hazard class (7) already names, and testing.md convention 14's target for
"a test whose result depends on incidental state." The label emission is
instead a static property of the checked-in Dockerfile: whether it can
silently stop being emitted needs no image build, no container, and no
network — same reasoning as `test_s6_services_are_supervised.py`.
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_REPO_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
)
_DOCKERFILE = os.path.join(_REPO_ROOT, "Dockerfile")


def _dockerfile_text():
    with open(_DOCKERFILE, encoding="utf-8") as fh:
        return fh.read()


def test_the_final_stage_declares_the_build_commit_arg():
    """Guard the guard: if the ARG this label reads were ever renamed or
    dropped, the label assertion below would either false-pass on stale text
    or need to be read against the wrong name."""
    text = _dockerfile_text()
    assert re.search(r"^ARG FAUNA_BUILD_COMMIT=", text, re.MULTILINE), (
        "Dockerfile no longer declares ARG FAUNA_BUILD_COMMIT — the revision "
        "label below has nothing to read"
    )


def test_the_image_carries_a_revision_label_sourced_from_the_build_commit():
    """The OCI label must exist and must read the SAME value the release-
    candidate gate already reads off `.Config.Env` — two labels naming two
    different sources of truth is worse than one, since they can silently
    diverge."""
    text = _dockerfile_text()
    match = re.search(
        r"^LABEL\b.*org\.opencontainers\.image\.revision=\$\{FAUNA_BUILD_COMMIT\}",
        text,
        re.MULTILINE,
    )
    assert match, (
        "Dockerfile must stamp `org.opencontainers.image.revision="
        "${FAUNA_BUILD_COMMIT}` so `docker inspect --format "
        '\'{{index .Config.Labels "org.opencontainers.image.revision"}}\'` '
        "answers which commit an artifact was built from without a "
        "behavioural inference (testing.md class (7))"
    )
