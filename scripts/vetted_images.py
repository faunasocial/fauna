"""The vetted third-party container images — the one table the release pipeline reads.

Every third-party image a Fauna bundle pulls, and every base image the
Dockerfile builds from, is pinned by digest and recorded here with the evidence
it was vetted on (docs/goal/architecture/release-integrity.md § Third-party
container images). Stdlib only, so the pipeline runs it with no dependencies:

    uv run --no-project python scripts/vetted_images.py --print-mirror-images
        one `<fauna-namespace repo>\t<digest-pinned source>` line per image the
        release pipeline mirrors (scripts/mirror-third-party-images.sh,
        scripts/bundle-scanner-probe.py);
    uv run --no-project python scripts/vetted_images.py --check-image-currency
        a non-blocking report of every pin against its live tag.

The invariants over this table — every shipping copy names a vetted ref, every
vetted ref covers every architecture the nest image is published for — are
asserted by the installer test suite beside this file (test_install_fauna.py).
"""

from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass


# To vet a new ref or bump a digest: `docker buildx imagetools inspect <name:tag>`
# (registry metadata only — no image pulled, no dependency source read); record
# the index `Digest:` line, its linux platforms, whether attestation manifests
# (the `unknown/unknown` entries) accompany it, and the date; then update every
# shipping copy the service maps to below. Never guess from the tag name.
# Scrutiny classes per the owner doc: a scanner bump is the in-range class; a
# docker.sock holder (watchtower) is reviewed like a major crate bump.


@dataclass(frozen=True)
class VettedImage:
    arches: frozenset[str]  # linux architectures the manifest index publishes
    publisher: str  # "project-official" | "personal-fork"
    attestations: bool  # attestation manifests accompany the index (recorded, not verified)
    vetted: str  # YYYY-MM-DD the digest was resolved and the index inspected
    # The repo name under ghcr.io/faunasocial/ the release pipeline re-publishes
    # this digest as (owner doc part 4 — the Fauna-namespace mirror). None for a
    # ref that is only a BUILD INPUT (a Dockerfile FROM), which the pipeline
    # consumes on the runner and never ships a reference to: mirroring one would
    # publish an image no box ever pulls.
    mirror: str | None = None


_CLAMAV = (
    "clamav/clamav:latest-debian"
    "@sha256:967334b92d1782e4d1314ddf903ae537d26792d21c9a39adecb8ac9757980514"
)
_RSPAMD = (
    "rspamd/rspamd:latest"
    "@sha256:86bc544548bc881276e19dcff4cf36bc4fb5c8a3050717f3c2315cb823938a80"
)
_WATCHTOWER = (
    "nickfedor/watchtower:latest"
    "@sha256:3b8d2e3f0f6ff9295a5d634e4fdb7062e5e6602f71d89baa15cd4d39d22b5743"
)

# The Dockerfile's own base/build-stage images — BUILD INPUTS, not something a
# shipped bundle pulls (they run only on the build runner). Same vetted table,
# same digest-pin discipline, but `mirror=None`: the release pipeline consumes
# these on the runner and never ships a reference to them (see VettedImage.mirror).
# release-integrity.md § Third-party container images, the build-input half.
_RUST_BASE = (
    "rust:bookworm"
    "@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922"
)
_GOLANG_BASE = (
    "golang:1.26.9-bookworm"
    "@sha256:d9c68c2c51161e12fd77e4c6320687c9cd86e1af1e3ad6e6cd63ff970641453c"
)
_DENO_BASE = (
    "denoland/deno:latest"
    "@sha256:2014dc167ece617ef7e7ba40631ac2234c59e75ce693e7cc2dc2602b3c87859d"
)
_DEBIAN_BASE = (
    "debian:bookworm-slim"
    "@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171"
)

# ref (name:tag@digest) -> its vetting evidence. Measured against the live
# registry 2026-08-27 (2026-08-29 for the four Dockerfile build inputs below;
# the golang entry re-vetted 2026-10-11 with the go1.26.9 toolchain pin bump).
_VETTED_THIRD_PARTY_IMAGES: dict[str, VettedImage] = {
    _CLAMAV: VettedImage(
        frozenset({"amd64", "arm64", "ppc64le"}),
        "project-official",
        True,
        "2026-08-27",
        mirror="clamav",
    ),
    _RSPAMD: VettedImage(
        frozenset({"amd64", "arm64"}), "project-official", True, "2026-08-27", mirror="rspamd"
    ),
    # Personal-namespace fork of the unmaintained containrrr/watchtower (whose
    # Docker client is rejected by Docker 24+ daemons). The weakest publisher
    # class in the table and the one component holding docker.sock — the owner
    # doc's part 4 (the Fauna-namespace mirror) is what retires the dependence.
    _WATCHTOWER: VettedImage(
        frozenset({"amd64", "386", "arm/v6", "arm64", "riscv64"}),
        "personal-fork",
        True,
        "2026-08-27",
        mirror="watchtower",
    ),
    # Docker Official Image (`library/rust`) — carries attestation manifests.
    _RUST_BASE: VettedImage(
        frozenset({"amd64", "arm/v7", "arm64", "386", "ppc64le"}),
        "project-official",
        True,
        "2026-08-29",
    ),
    # Docker Official Image (`library/golang`). Kept in lockstep with the Go
    # toolchain pin — build-system.md § Go toolchain pin names this table as
    # the second thing a pin bump updates.
    _GOLANG_BASE: VettedImage(
        frozenset({"amd64", "arm/v7", "arm64", "386", "ppc64le"}),
        "project-official",
        True,
        "2026-10-11",
    ),
    # The Deno project's own account (not the Docker Official Images program) —
    # no attestation manifests accompany this index.
    _DENO_BASE: VettedImage(
        frozenset({"amd64", "arm64"}),
        "project-official",
        False,
        "2026-08-29",
    ),
    # Docker Official Image (`library/debian`) — carries attestation manifests.
    _DEBIAN_BASE: VettedImage(
        frozenset({"amd64", "arm/v7", "arm64", "386", "ppc64le"}),
        "project-official",
        True,
        "2026-08-29",
    ),
}

# The Fauna namespace every mirrored digest is re-published under, and the one
# registry prefix a shipping bundle may name once part 4's flip has landed.
_FAUNA_NAMESPACE = "ghcr.io/faunasocial"


def _mirror_plan() -> list[dict[str, str]]:
    """The pipeline's mirror worklist: one entry per vetted ref to re-publish.

    `source` is the digest-pinned third-party ref — what `imagetools create`
    copies FROM, so the mirror can only ever carry bytes a human vetted. `repo`
    is the Fauna-namespace repository it lands in. Consumed by
    build-nest-image.yml / restage-nest.yml through --print-mirror-images, so
    the vetted table stays the single source of which digests are mirrored.
    """
    return [
        {"repo": f"{_FAUNA_NAMESPACE}/{v.mirror}", "source": ref}
        for ref, v in sorted(_VETTED_THIRD_PARTY_IMAGES.items())
        if v.mirror
    ]


# ⚠ clamav/clamav:latest (and every other UNSUFFIXED clamav tag) is amd64-ONLY.
# Recorded here, deliberately un-vetted, so that a future edit cannot re-admit
# it without meeting the named reason.
_KNOWN_BAD_TAGS: dict[str, str] = {
    "clamav/clamav:latest": "amd64-only (every unsuffixed clamav tag is)",
}


_INDEX_DIGEST_RE = re.compile(r"^Digest:\s*(sha256:[0-9a-f]{64})", re.MULTILINE)


def _live_index_digest(name_tag: str) -> str | None:
    """The live manifest-index digest for `name:tag`, via a registry metadata read.

    Never `docker pull` — `imagetools inspect` reads only registry metadata
    (release-integrity.md § Third-party container images part 2). Returns None
    if the probe itself failed (network/registry hiccup), which the caller
    reports as unresolvable rather than drifted.
    """
    proc = subprocess.run(
        ["docker", "buildx", "imagetools", "inspect", name_tag],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        print(f"  probe failed for {name_tag}: {proc.stderr.strip()}", file=sys.stderr)
        return None
    m = _INDEX_DIGEST_RE.search(proc.stdout)
    return m.group(1) if m else None


def check_image_currency() -> int:
    """Print a visible, non-blocking currency report for every vetted image.

    Always exits 0: currency is reviewed on the monthly cadence
    (release-integrity.md § Dependency currency, extended to images by §
    Third-party container images part 3) — never auto-bumped, so a drifted
    digest is a `::warning::` GitHub Actions annotation, never a red job. The
    monthly review reads this report instead of re-resolving every ref by hand.
    """
    drifted = unresolved = 0
    for ref, vetted in sorted(_VETTED_THIRD_PARTY_IMAGES.items()):
        name_tag, pinned_digest = ref.rsplit("@", 1)
        live_digest = _live_index_digest(name_tag)
        if live_digest is None:
            unresolved += 1
            print(f"UNRESOLVED {name_tag}: registry probe failed, see stderr")
        elif live_digest != pinned_digest:
            drifted += 1
            print(
                f"::warning title=Third-party image currency::{name_tag} has drifted — "
                f"pinned {pinned_digest}, live {live_digest} (last vetted {vetted.vetted}). "
                "Reviewed on the monthly currency cycle, never auto-bumped "
                "(release-integrity.md § Dependency currency)."
            )
        else:
            print(f"current    {name_tag} (last vetted {vetted.vetted})")
    total = len(_VETTED_THIRD_PARTY_IMAGES)
    print(f"— {total} vetted image(s): {drifted} drifted, {unresolved} unresolved, "
          f"{total - drifted - unresolved} current.")
    return 0


if __name__ == "__main__":
    # Not a test runner: the two non-pytest entry points, so the release
    # pipeline / currency job read the vetted table without keeping a second list.
    if sys.argv[1:] == ["--print-mirror-images"]:
        for entry in _mirror_plan():
            print(f"{entry['repo']}\t{entry['source']}")
    elif sys.argv[1:] == ["--check-image-currency"]:
        raise SystemExit(check_image_currency())
    else:
        print(__doc__, file=sys.stderr)
        raise SystemExit(2)
