"""Pure planning helpers for the **kept staging box** live test
(``tests/live/test_staging_box_provision.py``).

Everything here is a function of its arguments — no network, no driver — so
the decisions that would be expensive to get wrong against a real, paid, kept
box are pinned by a tier_1 test (``tests/test_staging_box_plan.py``) instead of
being discovered mid-provision:

  * which RRsets in the zone are the box's own (:func:`rrsets_under` — the
    whole subtree for a box on a subdomain; for a box at a zone apex, what the
    nest's record matrix publishes as ``fixtures/box-dns-names.json`` lists
    it, never the zone's NS/SOA or a name the box does not publish);
  * what the run does when it finds provider resources at the box's name
    (:func:`decide_start`, over :func:`server_name_for` and :func:`rrsets_under`),
    and what a failed run removes (:func:`cleanup_plan`);
  * where the box's admin credentials live between runs
    (:func:`load_or_create_state`);
  * which update channel the box follows, and the image tag that channel
    pulls (:func:`staging_channel`, :func:`image_tag_for`).
"""
from __future__ import annotations

import json
import os
import re
import secrets
from pathlib import Path

#: Provider-side label the kept box carries. Deliberately NOT the throwaway
#: ``fauna-e2e=1`` label: every live test's teardown deletes any server with
#: that label once it is an hour old (``tests/live/conftest.py``'s orphan sweep).
STAGING_LABEL_KEY = "fauna-staging"
STAGING_LABEL_VALUE = "1"

#: Per-machine home of the kept box's admin credentials (seed, handle, mail
#: password). Outside every repository, like the provider token's own file.
STATE_DIR = "~/.config/fauna/staging-box"

#: :func:`decide_start` verdicts.
PROVISION = "provision"
RESUME = "resume"


class StagingPlanError(ValueError):
    """The run cannot proceed from what it found — raised BEFORE anything is
    provisioned, so the refusal costs nothing."""


#: The one place a channel becomes an image tag (`UpdateChannel::image_tag`).
#: Read, never restated: a Python copy of the table would be a second one.
_UPDATE_CHANNEL_RS = (Path(__file__).resolve().parents[3]
                      / "libs" / "fauna-provisioning" / "src" / "cloud_init.rs")


def _match_arms(source: str, fn: str) -> dict[str, str]:
    """``Self::Variant => "value"`` arms of ``pub fn <fn>(self)``'s match."""
    m = re.search(rf"pub fn {fn}\(self\)[^{{]*\{{(.*?)\n    \}}", source, re.S)
    if not m:
        raise StagingPlanError(f"no `UpdateChannel::{fn}` in {_UPDATE_CHANNEL_RS}")
    return dict(re.findall(r'Self::(\w+)\s*=>\s*"([^"]+)"', m.group(1)))


def update_channel_tags() -> dict[str, str]:
    """Channel id (the wizard's row key) → the image tag the box follows, as
    ``UpdateChannel::id`` and ``UpdateChannel::image_tag`` map them."""
    source = _UPDATE_CHANNEL_RS.read_text()
    ids, tags = _match_arms(source, "id"), _match_arms(source, "image_tag")
    if not ids or ids.keys() != tags.keys():
        raise StagingPlanError(
            f"`UpdateChannel::id` {ids} and `::image_tag` {tags} do not cover the "
            f"same variants in {_UPDATE_CHANNEL_RS}"
        )
    return {ids[v]: tags[v] for v in ids}


def staging_channel(raw: str | None) -> str | None:
    """The update channel a run names (``FAUNA_E2E_STAGING_CHANNEL``), or None
    when it names none. An unknown name is refused, never guessed — ``latest``
    is a tag, not a channel."""
    name = (raw or "").strip().lower()
    if not name:
        return None
    channels = update_channel_tags()
    if name not in channels:
        raise StagingPlanError(
            f"unknown update channel {raw!r}: one of {sorted(channels)} "
            "(the wizard's channel, not an image tag)"
        )
    return name


def image_tag_for(channel: str) -> str:
    """The image tag a box on ``channel`` pulls and its updater follows."""
    return update_channel_tags()[channel]


def nest_image_ref(tag: str) -> str:
    """The nest image a freshly provisioned box pulls on ``tag``, read from
    the cloud-init compose template (``image: <repo>:{image_tag}``) — never
    restated, so a moved repository moves this with it."""
    source = _UPDATE_CHANNEL_RS.read_text()
    m = re.search(r"image:\s*(\S+):\{image_tag\}", source)
    if not m:
        raise StagingPlanError(
            f"no `image: <repo>:{{image_tag}}` line in {_UPDATE_CHANNEL_RS}"
        )
    return f"{m.group(1)}:{tag}"


def anonymous_pull_urls(image_ref: str) -> tuple[str, str]:
    """The registry's anonymous pull-token URL and the manifest URL for
    ``image_ref`` (``<registry>/<repository>:<tag>``) — the two reads a box's
    unauthenticated ``docker pull`` makes first. A box carries no registry
    credential, so an image those two reads cannot fetch is one a box built
    now would never start (the 2026-10-12 dev.fauna.build run: 20 min of a
    paid box answering nothing, the package having been deleted)."""
    name, _, tag = image_ref.rpartition(":")
    registry, _, repository = name.partition("/")
    if not (registry and repository and tag) or "." not in registry:
        raise StagingPlanError(f"not a <registry>/<repository>:<tag> reference: {image_ref!r}")
    return (
        f"https://{registry}/token?scope=repository:{repository}:pull&service={registry}",
        f"https://{registry}/v2/{repository}/manifests/{tag}",
    )


def channel_row_id(channel: str) -> str:
    """The wizard's row for ``channel`` on ``vps_config``."""
    return f"vps-config-update-channel-row[{channel}]"


def server_name_for(domain: str) -> str:
    """The server name the provisioning orchestrator gives a box for ``domain``
    (it replaces dots with dashes)."""
    return domain.replace(".", "-")


def zone_for(domain: str, zone_names: list[str]) -> str | None:
    """The longest of ``zone_names`` that ``domain`` sits in (or equals)."""
    domain = domain.rstrip(".").lower()
    best = None
    for name in zone_names:
        z = name.rstrip(".").lower()
        if domain == z or domain.endswith("." + z):
            if best is None or len(z) > len(best):
                best = z
    return best


def no_zone_message(domain: str, zone_names: list[str]) -> str:
    """The preflight's failure when ``domain`` sits in none of the token's
    zones. A box in the uat zone names the set-up step: the uat project must
    hold the zone ``fauna.build`` and the registrar must point the domain's
    nameservers at the provider's (``org-nests.md`` § One zone, one project)."""
    from helpers.live_provision import cloud_project

    domain = domain.rstrip(".").lower()
    msg = (f"{domain!r} is in none of the token's DNS zones ({sorted(zone_names)}) — "
           "wrong token/project, or wrong name")
    if cloud_project.project_for(domain) == cloud_project.UAT:
        msg += (f". {domain} belongs to the uat project: the token must be that project's "
                f"({cloud_project.token_file(cloud_project.UAT)}), the project must hold the "
                f"zone {cloud_project.UAT_ZONE}, and the registrar must point that domain's "
                f"nameservers at the provider's nameservers for the zone (the set-up step)")
    return msg


def relative_name(domain: str, zone: str) -> str:
    """``domain`` as a zone-relative RRset name (``@`` for the apex)."""
    domain, zone = domain.rstrip(".").lower(), zone.rstrip(".").lower()
    if domain == zone:
        return "@"
    return domain[: -len(zone) - 1]


#: The RRsets a nest's record matrix publishes, by owner name relative to the
#: box's domain. Pinned to the real matrix by a nest test
#: (``dns_handlers::tests::box_dns_names_fixture_is_the_matrix``), so no
#: hand-kept copy of the matrix lives here.
_BOX_DNS_NAMES = Path(__file__).resolve().parents[1] / "fixtures" / "box-dns-names.json"


def box_rrset_shapes() -> list[dict]:
    """``{"name", "type"[, "value_prefix"]}`` for every RRset a box publishes
    at its own domain (``@``) and below it; ``*`` in a name is one label,
    save in a ``"literal": true`` shape, whose ``*`` is the DNS wildcard label."""
    return json.loads(_BOX_DNS_NAMES.read_text(encoding="utf-8"))["rrsets"]


def _name_matches(shape: dict, name: str) -> bool:
    if shape.get("literal"):
        return shape["name"].lower() == name.lower()
    want, got = shape["name"].lower().split("."), name.lower().split(".")
    return len(want) == len(got) and all(w == "*" or w == g for w, g in zip(want, got))


def _txt_text(value: str) -> str:
    """A TXT value as text: providers hand it back quoted (``"v=spf1 mx ~all"``)."""
    return value.strip().lstrip('"')


def _apex_box_rrsets(rrsets: list[dict]) -> list[dict]:
    shapes = box_rrset_shapes()
    out = []
    for rs in rrsets:
        name, rtype = rs.get("name") or "", (rs.get("type") or "").upper()
        shape = next((s for s in shapes
                      if s["type"] == rtype and _name_matches(s, name)), None)
        if shape is None:
            continue
        prefix = shape.get("value_prefix")
        if prefix is None:
            out.append(rs)
            continue
        # A name the box shares with the zone (the apex TXT): only the box's
        # values are its, and the RRset is handed on holding just those.
        own = [r for r in rs.get("records", [])
               if _txt_text(r.get("value", "")).startswith(prefix)]
        if own:
            out.append({**rs, "records": own})
    return out


def rrsets_under(rrsets: list[dict], rel: str) -> list[dict]:
    """The RRsets — out of the zone's ``rrsets`` — that belong to the box named
    ``rel`` (zone-relative).

    * A box on a subdomain owns its whole subtree: the name itself and
      everything below it (``mail.<rel>``, ``_dmarc.<rel>``,
      ``<selector>._domainkey.<rel>``).
    * A box at the zone apex (``@``) shares its name with the zone, so it owns
      exactly what the nest's record matrix publishes
      (:func:`box_rrset_shapes`): the apex's A/AAAA/MX, the SPF value of the
      apex TXT (that RRset comes back holding the SPF value alone), and the
      matrix's names below the apex. Never the zone's NS or SOA, and never a
      name the box does not publish."""
    if rel == "@":
        return _apex_box_rrsets(rrsets)
    suffix = "." + rel
    return [r for r in rrsets if r.get("name") == rel or r.get("name", "").endswith(suffix)]


def decide_start(*, state: dict, servers: list[dict], rrsets: list[dict], rel: str) -> str:
    """What a run does given what already exists at the box's name.

    ``rrsets`` are the zone's, and only the box's own (:func:`rrsets_under`
    ``rel``) count: at a zone apex the zone always holds its NS and SOA, so
    "nothing exists" means none of the box's own RRsets do.

    * nothing exists                     → :data:`PROVISION`
    * the server this machine's state file records as provisioned-and-claimed
      exists                              → :data:`RESUME` (sign in, finish the
      remaining steps — every one of them is idempotent)
    * anything else                       → refuse. A server this state file did
      not record is either someone else's box or one whose provisioning died
      before the claim; a run that provisioned over it would mint a fresh
      identity for a box that already has one. Stray RRsets with no server
      would be silently adopted as the new box's records.

    A START never deletes. A run that FAILS before the claim removes what it
    just created (`cleanup_plan`); anything found here that it does not
    recognise is left for a person.
    """
    rrsets = rrsets_under(rrsets, rel)
    if not servers and not rrsets:
        return PROVISION
    recorded = state.get("server_id")
    if recorded is not None and any(s.get("id") == recorded for s in servers):
        return RESUME
    raise StagingPlanError(
        f"{state.get('domain')} already exists, in whole or in part, and this "
        "machine's state file does not record it as a box this test provisioned "
        f"and claimed (recorded server id: {recorded!r}). Found servers "
        f"{[(s.get('id'), s.get('name')) for s in servers]} and RRsets "
        f"{[(r.get('name'), r.get('type')) for r in rrsets]}. This test never "
        "replaces or deletes a box: remove them deliberately, then rerun."
    )


def cleanup_plan(*, state: dict, servers: list[dict], rrsets: list[dict], rel: str) -> dict:
    """What a FAILED run removes: the server and records at the box's name —
    but only while the state file does not record the box as built and
    claimed.

    ``rrsets`` are the zone's; the plan takes only the box's own
    (:func:`rrsets_under` ``rel``), so at a zone apex it can never name the
    zone's NS or SOA or a record the box does not publish, and of the apex TXT
    it names the SPF value alone. This is production DNS.

    * not recorded (`server_id` absent): the run failed before the claim. The
      box can never be claimed — its claim code lived in the app instance that
      just ended — so it is dead weight that would block every rerun. Remove
      the server(s) and RRsets at the name. `decide_start` proved, before this
      run created anything, that nothing was there, so everything found is
      this run's own.
    * recorded: the box is claimed and a rerun resumes it. Remove nothing.

    This is cleanup, never a route to green: it is only ever called from a
    failing run's `except` branch, which re-raises.
    """
    if state.get("server_id") is not None:
        return {"servers": [], "rrsets": []}
    return {"servers": list(servers), "rrsets": rrsets_under(rrsets, rel)}


def state_path(domain: str) -> Path:
    return Path(os.path.expanduser(STATE_DIR)) / f"{domain}.json"


def load_or_create_state(domain: str, *, localpart: str = "admin") -> dict:
    """The kept box's admin credentials, created on first use and reused after.

    Written BEFORE anything is provisioned and with mode 0600: the seed is the
    only key to a box the run then claims, so a crash between the claim and a
    later write would strand a paid, claimed, unreachable-as-admin server. A
    rerun reuses the same identity.
    """
    path = state_path(domain)
    if path.exists():
        state = json.loads(path.read_text(encoding="utf-8"))
        if state.get("domain") != domain or not state.get("secret_hex"):
            raise StagingPlanError(
                f"{path} does not describe {domain!r} (or holds no seed); move "
                "it aside rather than letting a run overwrite it"
            )
        return state
    state = {
        "domain": domain,
        "handle": f"{localpart}@{domain}",
        "secret_hex": secrets.token_bytes(32).hex(),
        "mail_password": secrets.token_urlsafe(24),
    }
    save_state(state)
    return state


def save_state(state: dict) -> Path:
    path = state_path(state["domain"])
    path.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(state, f, indent=2, sort_keys=True)
        f.write("\n")
    os.chmod(path, 0o600)
    return path
