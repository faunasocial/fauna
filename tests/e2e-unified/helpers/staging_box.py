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
    pulls (:func:`staging_channel`, :func:`image_tag_for`);
  * whether a dev machine may make a complex connection to the acceptance
    nest now (:func:`admit_complex_connection`, over the re-creation record
    :func:`record_recreation` writes).
"""
from __future__ import annotations

import json
import os
import re
import secrets
from datetime import datetime, timedelta
from pathlib import Path

from helpers.nest_mode import ACCEPTANCE_NEST_HOSTS

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
                      / "libs" / "fauna-provisioning" / "src" / "update_channel.rs")

#: The cloud-init compose template, whose `image:` line names the repository.
_CLOUD_INIT_RS = _UPDATE_CHANNEL_RS.with_name("cloud_init.rs")


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
    source = _CLOUD_INIT_RS.read_text()
    m = re.search(r"image:\s*(\S+):\{image_tag\}", source)
    if not m:
        raise StagingPlanError(
            f"no `image: <repo>:{{image_tag}}` line in {_CLOUD_INIT_RS}"
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
    zones. A box in the organisation's zone names the set-up step: the zone
    project must hold the zone ``fauna.build`` and the registrar must point the
    domain's nameservers at the provider's (``org-nests.md`` § One zone, one
    project)."""
    from helpers.live_provision import cloud_project

    domain = domain.rstrip(".").lower()
    msg = (f"{domain!r} is in none of the token's DNS zones ({sorted(zone_names)}) — "
           "wrong token/project, or wrong name")
    if cloud_project.project_for(domain, purpose=cloud_project.DNS) == cloud_project.ZONE:
        msg += (f". {domain}'s records belong to the zone project: the DNS token must be "
                f"that project's ({cloud_project.token_file(cloud_project.ZONE)}), the project "
                f"must hold the zone {cloud_project.ORG_ZONE}, and the registrar must point "
                f"that domain's nameservers at the provider's nameservers for the zone (the "
                f"set-up step)")
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


def is_recorded_server(server: dict, recorded) -> bool:
    """Whether the provider's ``server`` is the one the state file records.

    The state file takes its id from the provisioning snapshot, which carries
    it as a string; the Cloud API lists servers by integer id. Compare the text."""
    return recorded is not None and server.get("id") is not None \
        and str(server["id"]) == str(recorded)


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
    if any(is_recorded_server(s, recorded) for s in servers):
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


# ── The fresh-box guard: one complex connection per re-creation ─────────────
#
# `org-nests.md` § fauna.build: a dev machine makes a complex connection to the
# acceptance nest — anything beyond a plain HTTPS probe: a sign-in, a seeded
# load, a log pull — only to a box re-created since its last such connection,
# because a box a stranger can hold may already have fallen, and a fresh box
# from the signed image is the only one a dev machine trusts that far. The
# record lives in the box's state file beside its identity, so the machine that
# re-creates the box is the machine that can connect to it.

#: The boxes the guard holds to it — the acceptance nest alone, the one set
#: the live harness also refuses outright. A staging or throwaway box takes
#: its complex connections without one.
FRESH_BOX_HOSTS = ACCEPTANCE_NEST_HOSTS

#: The calendar half of the rule: a box re-created longer ago than this is
#: overdue its weekly re-creation, and takes no complex connection until it has
#: had one. A constant in the tree, never a setting (`org-nests.md` § fauna.build).
RECREATION_CADENCE = timedelta(days=7)


class StaleBoxError(StagingPlanError):
    """A complex connection to a box that has not been re-created since the
    last one — raised BEFORE anything connects, so the refusal costs nothing."""


def _instant(value: str, what: str) -> datetime:
    """An ISO-8601 time with its zone, or a refusal — the guard never admits
    a connection on a time it cannot read."""
    try:
        when = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except (AttributeError, ValueError):
        when = None
    if when is None or when.tzinfo is None:
        raise StagingPlanError(f"{what} {value!r} is not an ISO-8601 time with a zone")
    return when


def record_recreation(state: dict, *, at: str, image: str) -> dict:
    """``state`` with a re-creation recorded: when it finished and the image
    the box serves, and no complex connection made since.

    Written once, at the END of a re-creation — after the box serves, its
    accounts sign in and its firewall is verified. Every connection the
    re-creation itself makes (the claim, the re-admissions, the seeding) is
    the re-creation, not a connection to a box that has been running, so none
    of them consumes the record.
    """
    if not at or not image:
        raise StagingPlanError("a re-creation record needs its time and the image it serves")
    out = {**state, "recreated_at": at, "recreated_image": image, "complex_connection": None}
    out.pop("recreation_started_at", None)
    return out


def admit_complex_connection(state: dict, *, purpose: str, at: str) -> dict:
    """``state`` with this complex connection recorded — or a refusal.

    For a box outside :data:`FRESH_BOX_HOSTS`, ``state`` unchanged. For the
    acceptance nest: refused when no re-creation is recorded, when the last one
    is older than :data:`RECREATION_CADENCE`, or when a complex connection has
    already been made since it. The caller
    saves the returned state BEFORE it connects, so a run that dies mid-way
    has still used the box up — the failure leaves a refusal behind, never a
    second connection to a box that has been talked to.
    """
    domain = (state.get("domain") or "").lower().rstrip(".")
    if domain not in FRESH_BOX_HOSTS:
        return state
    if not purpose or not at:
        raise StagingPlanError("a complex connection names its purpose and time")
    recreated = state.get("recreated_at")
    if not recreated:
        raise StaleBoxError(
            f"refusing {purpose} to {domain}: no re-creation of it is recorded on "
            "this machine. A complex connection goes only to a box re-created "
            "since the last one (org-nests.md § fauna.build) — re-create it first."
        )
    age = _instant(at, "the connection time") - _instant(recreated, "the re-creation time")
    if age > RECREATION_CADENCE:
        raise StaleBoxError(
            f"refusing {purpose} to {domain}: its last re-creation, at {recreated}, "
            f"is older than the {RECREATION_CADENCE.days}-day cadence and the box is "
            "overdue its calendar re-creation. Re-create it first "
            "(org-nests.md § fauna.build)."
        )
    used = state.get("complex_connection")
    if used:
        raise StaleBoxError(
            f"refusing {purpose} to {domain}: it was re-created at {recreated} and "
            f"has had its one complex connection since ({used.get('purpose')} at "
            f"{used.get('at')}). Re-create it first (org-nests.md § fauna.build)."
        )
    return {**state, "complex_connection": {"purpose": purpose, "at": at}}


def admit_drive_resume(state: dict, *, at: str) -> dict:
    """The provisioning drive about to sign in to a box it finds already built.

    Mid re-creation (a teardown has started and no record closed it) the
    sign-in is the re-creation's own and ``state`` comes back unchanged. On a
    box that has been running, it is a complex connection like any other
    (:func:`admit_complex_connection`)."""
    if state.get("recreation_started_at"):
        return state
    return admit_complex_connection(state, purpose="provisioning drive resume", at=at)


# ── The re-creation of the acceptance nest ──────────────────────────────────
#
# 
#
# Teardown → the provisioning drive (a fresh box, its accounts re-admitted and
# re-seeded) → the box's own firewall re-applied → the serving gate → the
# record. The state file carries the re-creation across those steps, so a run
# that dies anywhere is re-run from the top: the teardown finds what is left.


def begin_recreation(*, state: dict, servers: list[dict], rrsets: list[dict],
                     rel: str, at: str) -> dict:
    """What the teardown removes, and the state it saves BEFORE removing it.

    Returns ``{"servers", "rrsets", "state"}``. The servers are those at the
    box's name — refused unless each is the one this machine's state file
    records (a box someone else built is never deleted); the RRsets are the
    box's own (:func:`rrsets_under`), never the zone's NS/SOA.

    The state keeps the admin identity and the mail password (minting one if
    a run dropped it — the drive turns mail on with it), remembers the ids of
    the firewalls applied to the old server (the box's own firewall outlives
    its server and is re-applied to the new one), voids the old re-creation
    record and marks the re-creation begun."""
    domain = (state.get("domain") or "").lower().rstrip(".")
    if domain not in FRESH_BOX_HOSTS:
        raise StagingPlanError(
            f"{domain!r} is not the acceptance nest: the calendar re-creation is "
            f"fauna.build's alone ({sorted(FRESH_BOX_HOSTS)})"
        )
    recorded = state.get("server_id")
    strangers = [s for s in servers if not is_recorded_server(s, recorded)]
    if strangers:
        raise StagingPlanError(
            f"refusing to tear down {domain}: this machine's state file does not "
            f"record the server(s) {[(s.get('id'), s.get('name')) for s in strangers]} "
            f"at its name (recorded: {recorded!r}). Removing them is a person's act."
        )
    firewall_ids = list(state.get("firewall_ids") or [])
    for s in servers:
        for fw in (s.get("public_net") or {}).get("firewalls", []):
            if fw.get("id") is not None and fw["id"] not in firewall_ids:
                firewall_ids.append(fw["id"])
    new = {**state, "recreation_started_at": at, "firewall_ids": firewall_ids,
           "complex_connection": None,
           "mail_password": state.get("mail_password") or secrets.token_urlsafe(24),
           "mail_enabled_by_this_test": False}
    new.pop("recreated_at", None)
    return {"servers": list(servers), "rrsets": rrsets_under(rrsets, rel), "state": new}


def finish_teardown(state: dict) -> dict:
    """``state`` once the old server and records are gone: no server recorded,
    so the drive's :func:`decide_start` provisions afresh."""
    return {**state, "server_id": None, "ipv4": None}


def recreation_record_refusal(state: dict) -> str | None:
    """Why the re-creation may not be recorded yet, or None when it may.

    The record is what admits the next complex connection, so it is written
    only once the drive has built a new box and re-admitted and re-seeded its
    accounts (``accounts_seeded_at``, written by the drive, later than the
    teardown's start). The firewall and the serving gate are checked by the
    caller, at the provider and on the wire."""
    started = state.get("recreation_started_at")
    if not started:
        return "no re-creation is in progress: run the teardown first"
    if state.get("server_id") is None:
        return "the provisioning drive provisioned no box since the teardown"
    seeded = state.get("accounts_seeded_at")
    if not seeded or _instant(seeded, "the seeding time") < _instant(started, "the teardown time"):
        return ("the provisioning drive has not re-admitted and re-seeded the reviewer and "
                "tester accounts since the teardown (no accounts_seeded_at after "
                f"{started})")
    return None
