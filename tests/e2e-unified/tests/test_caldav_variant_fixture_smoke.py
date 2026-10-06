"""Smoke test for the `unclaimed_caldav_nest` factory fixture (plan Task 2).

Builds, for a given address type, a fresh UNCLAIMED nest + an UNAPPROVED MTA +
a self-enrolling MDA (with a CalDAV listener port), and returns a
`CalDavVariantNestHandle`. The downstream onboarding-variant matrix (Task 4)
claims + approves the bridges through the client UI per cell; this smoke only
asserts the fixture assembles a live MDA against an unclaimed nest with the
address-type-correct handle domain.
"""

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]


@pytest.mark.parametrize(
    "address_type,expected_domain",
    [
        ("real_domain", "fauna.test"),
        ("localhost", "localhost"),
    ],
)
def test_unclaimed_caldav_nest_builds(
    unclaimed_caldav_nest, address_type, expected_domain
):
    h = unclaimed_caldav_nest(address_type)
    assert h.nest["admin"] is None  # unclaimed — the UI drives the claim
    assert h.domain == expected_domain or address_type == "ip"
    assert h.caldav_port and h.mda.proc.poll() is None  # MDA alive
