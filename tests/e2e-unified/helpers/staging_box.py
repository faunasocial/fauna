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
import sys
from datetime import datetime, timedelta, timezone
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
    return _write_private(state_path(state["domain"]), state)


def _write_private(path: Path, data: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, sort_keys=True)
        f.write("\n")
    os.chmod(path, 0o600)
    return path


# ── The acceptance nest's org-minted accounts ───────────────────────────────
#
# `org-nests.md` § fauna.build → *Accounts*: one reviewer account per store
# plus a tester, each identity minted on the dev host and kept beside the
# box's own, admitted by its public key through the admin UI — so the secret
# is never sent anywhere to create the account, and a re-created box admits
# the same keys again, keeping the secrets in the store consoles valid.

#: The org-minted accounts, by handle local part; the tester seeds the content
#: a reviewer meets on a first run. The acceptance nest holds them for the
#: stores; the two staging boxes hold the same set (each box its own
#: identities) so the store reviewer journey
#: (`tests/test_store_reviewer_journey.py`) runs where automated tests may
#: (owner, 2026-10-28; `org-nests.md` § fauna.build → *Accounts*).
_ORG_LOCALPARTS = ("microsoft-reviewer", "apple-reviewer", "google-reviewer", "tester")
ORG_ACCOUNTS = {
    "fauna.build": _ORG_LOCALPARTS,
    "dev.fauna.build": _ORG_LOCALPARTS,
    "test.fauna.build": _ORG_LOCALPARTS,
}
#: The account whose posts and messages populate the reviewers' first run.
SEEDING_ACCOUNT = "tester"
#: What the tester seeds for a reviewer's first run — plain content that
#: claims nothing about the product, so it is no public text.
SEED_POSTS = ("Hello from the tester account.",
              "A second post, so the feed has more than one.")
SEED_MESSAGE = "Hello — this conversation was started by the tester account."
#: The reviewer accounts among the org accounts, in store order.
REVIEWER_ACCOUNTS = tuple(lp for lp in _ORG_LOCALPARTS if lp != SEEDING_ACCOUNT)


def org_accounts_for(domain: str) -> tuple[str, ...]:
    return ORG_ACCOUNTS.get(domain, ())


def account_path(domain: str, localpart: str) -> Path:
    return Path(os.path.expanduser(STATE_DIR)) / domain / "accounts" / f"{localpart}.json"


def load_or_create_account(domain: str, localpart: str) -> dict:
    """An org-minted account's identity, minted once (mode 0600, before
    anything about it reaches the nest) and reused by every later run."""
    path = account_path(domain, localpart)
    if path.exists():
        account = json.loads(path.read_text(encoding="utf-8"))
        if (account.get("domain"), account.get("localpart")) != (domain, localpart) \
                or not account.get("secret_hex"):
            raise StagingPlanError(
                f"{path} does not describe {localpart} on {domain!r} (or holds no "
                "seed); move it aside rather than letting a run overwrite it")
        return account
    account = {"domain": domain, "localpart": localpart,
               "secret_hex": secrets.token_bytes(32).hex()}
    _write_private(path, account)
    return account


def actor_hex_for(secret_hex: str) -> str:
    """The actor id an imported seed signs in as: its Ed25519 public key."""
    from nacl.signing import SigningKey

    return bytes(SigningKey(bytes.fromhex(secret_hex)).verify_key).hex()


def mark_accounts_seeded(state: dict, *, at: str) -> dict:
    """``state`` once the drive has admitted every account and seeded the
    reviewers' content — the mark :func:`recreation_record_refusal` waits on."""
    return {**state, "accounts_seeded_at": at}


def account_actor(account: dict) -> str:
    """The actor id an account is admitted under: its ``actor_hex`` when the
    record carries only the public half (a witness host's tester, registered
    in `WITNESS_TESTER_REGISTRY`), else the public key of its seed."""
    if account.get("actor_hex"):
        return str(account["actor_hex"])
    return actor_hex_for(account["secret_hex"])


# ── The witness hosts' testers: secret-free, registered by public key ────────
#
# `org-nests.md` § fauna.build → *Accounts*, the witness-host tester: a store
# witness (`tests/artifact/test_*_store_install.py`) signs in to a staging box
# through the shipped app's own UI, so it needs an admitted identity on the
# machine it runs on — and the drive's identity files have one copy, on the dev
# host. Rather than carry a secret between machines, each witness host mints
# its OWN tester identity, keeps it where the drive keeps its own (0600 under
# `STATE_DIR`), and publishes only the public key and handle into the tracked
# registry below; the drive reads the registry on every run and admits each
# entry by public key, exactly as it admits the reviewers. The secret never
# leaves the machine that uses it, and a re-creation re-admits the same key.

#: The tracked registry — every witness host's tester, public half only.
WITNESS_TESTER_REGISTRY = Path(__file__).resolve().parents[1] / "org-testers.toml"
#: The staging boxes a witness tester may be registered for: the harness's
#: automated side of the shared-box rule, never the acceptance nest (humans
#: only — `testing.md` § The shared-box rule).
WITNESS_TESTER_DOMAINS = ("dev.fauna.build", "test.fauna.build")
#: The host platform a witness runs on → the tester's local part. One
#: witness machine per platform runs the store witnesses, so the platform
#: names the host, and the local part says what the account is for.
WITNESS_PLATFORMS = {"darwin": "macos", "win32": "windows", "linux": "linux"}


def witness_platform(platform: str | None = None) -> str:
    """This host's platform name for the tester's local part (``linux``,
    ``macos`` or ``windows``)."""
    platform = platform or sys.platform
    for prefix, name in WITNESS_PLATFORMS.items():
        if platform.startswith(prefix):
            return name
    return "linux"


def witness_tester_localpart(platform: str | None = None) -> str:
    """``<platform>-tester`` — the handle local part of this host's tester."""
    return f"{witness_platform(platform)}-tester"


def _validate_tester_entry(entry: dict, *, where: str) -> dict:
    domain = str(entry.get("domain") or "")
    localpart = str(entry.get("localpart") or "")
    actor = str(entry.get("actor_hex") or "")
    if domain not in WITNESS_TESTER_DOMAINS:
        raise StagingPlanError(
            f"{where}: {domain!r} is not a staging box a witness tester may be "
            f"registered for ({', '.join(WITNESS_TESTER_DOMAINS)})")
    if not localpart.endswith("-tester") or not (3 <= len(localpart) <= 63) \
            or not all(c.islower() or c.isdigit() or c == "-" for c in localpart) \
            or localpart.startswith("-"):
        raise StagingPlanError(f"{where}: {localpart!r} is not a witness tester's local part")
    if len(actor) != 64 or any(c not in "0123456789abcdef" for c in actor):
        raise StagingPlanError(f"{where}: {localpart}@{domain} carries no 64-hex actor id")
    return {"domain": domain, "localpart": localpart, "actor_hex": actor,
            "registered_at": str(entry.get("registered_at") or "")}


def load_witness_testers(registry: Path | None = None) -> list[dict]:
    """Every registered witness tester (``domain``, ``localpart``,
    ``actor_hex``, ``registered_at``), validated; an absent registry is empty."""
    import tomllib

    path = registry or WITNESS_TESTER_REGISTRY
    if not path.is_file():
        return []
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as e:
        raise StagingPlanError(f"{path}: not valid TOML ({e})") from None
    entries = [_validate_tester_entry(t, where=str(path)) for t in data.get("tester", ())]
    seen: set[tuple[str, str]] = set()
    for e in entries:
        key = (e["domain"], e["localpart"])
        if key in seen:
            raise StagingPlanError(f"{path}: {e['localpart']}@{e['domain']} is registered twice")
        seen.add(key)
    return entries


def witness_testers_for(domain: str, registry: Path | None = None) -> dict[str, dict]:
    """The registered testers of ``domain``, by local part — public half only,
    the shape :func:`account_actor` admits."""
    return {e["localpart"]: e for e in load_witness_testers(registry) if e["domain"] == domain}


def _write_witness_testers(entries: list[dict], registry: Path) -> None:
    lines = [
        "# The witness hosts' testers on the staging boxes — PUBLIC halves only.",
        "#",
        "# Each entry is an account a store-install witness signs in as, through",
        "# the shipped app's own UI, on the staging box named. The identity was",
        "# minted on the machine that runs the witness and its secret stays",
        "# there (`just org-tester-mint`); the staging-box provisioning drive",
        "# reads this file on every run and admits each entry by its public key,",
        "# as it admits the reviewers (`org-nests.md` § fauna.build → Accounts,",
        "# the witness-host tester). Written by the recipe, never by hand.",
        "",
    ]
    for e in sorted(entries, key=lambda e: (e["domain"], e["localpart"])):
        lines += ["[[tester]]",
                  f'domain = "{e["domain"]}"',
                  f'localpart = "{e["localpart"]}"',
                  f'actor_hex = "{e["actor_hex"]}"',
                  f'registered_at = "{e["registered_at"]}"',
                  ""]
    registry.write_text("\n".join(lines), encoding="utf-8")


def register_witness_tester(domain: str, *, platform: str | None = None,
                            registry: Path | None = None, now: str | None = None) -> dict:
    """Mint this host's tester identity for ``domain`` (once; reused after) and
    put its public half in the registry. Returns the registry entry plus
    ``changed``, whether the registry was written. The secret is written only
    under `STATE_DIR` on this machine."""
    if domain not in WITNESS_TESTER_DOMAINS:
        raise StagingPlanError(
            f"{domain!r} is not a staging box a witness tester may be registered for "
            f"({', '.join(WITNESS_TESTER_DOMAINS)}): the acceptance nest is humans-only")
    path = registry or WITNESS_TESTER_REGISTRY
    localpart = witness_tester_localpart(platform)
    account = load_or_create_account(domain, localpart)
    actor = actor_hex_for(account["secret_hex"])
    stamp = now or datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    entries = load_witness_testers(path)
    for e in entries:
        if (e["domain"], e["localpart"]) == (domain, localpart):
            if e["actor_hex"] == actor:
                return {**e, "changed": False}
            # The kept identity differs from the registered one: a kept file
            # moved aside and minted anew, or a registry written on another
            # machine of the same platform. The machine holding the secret
            # is the one the entry describes.
            e["actor_hex"], e["registered_at"] = actor, stamp
            _write_witness_testers(entries, path)
            return {**e, "changed": True}
    entry = {"domain": domain, "localpart": localpart, "actor_hex": actor, "registered_at": stamp}
    _write_witness_testers([*entries, entry], path)
    return {**entry, "changed": True}


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

#: The certificate budget (`org-nests.md` § fauna.build → *Disposable*, rule
#: (3)): every box bought requests a certificate for the box's exact name set,
#: and the issuer allows five per set per seven days. A buy is recorded in
#: ``state["box_buys"]`` before the drive presses "Buy and set up"
#: (:func:`note_box_bought`), and the teardown refuses a re-creation while the
#: window already holds the budget (:func:`certificate_budget_refusal`).
#: Constants in the tree, never settings.
CERTIFICATE_BUDGET = 5
CERTIFICATE_WINDOW = timedelta(days=7)


class StaleBoxError(StagingPlanError):
    """A complex connection to a box that has not been re-created since the
    last one — raised BEFORE anything connects, so the refusal costs nothing."""


class CertificateBudgetWait(StagingPlanError):
    """A re-creation refused because the certificate window is spent: a WAIT
    until the oldest recorded buy falls out of :data:`CERTIFICATE_WINDOW`."""


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


def record_recreation(state: dict, *, at: str, image: str,
                      version: str | None = None) -> dict:
    """``state`` with a re-creation recorded: when it finished, the image the
    box serves and that image's product version, and no complex connection
    made since.

    ``version`` is what the calendar's store-review hold matches a submission
    against (:func:`recreation_verdict`); the state file names the box by
    digest, which no submission carries. The re-creation's record step
    always passes it, read off the image's version label.

    Written once, at the END of a re-creation — after the box serves, its
    accounts sign in and its firewall is verified. Every connection the
    re-creation itself makes (the claim, the re-admissions, the seeding) is
    the re-creation, not a connection to a box that has been running, so none
    of them consumes the record.
    """
    if not at or not image:
        raise StagingPlanError("a re-creation record needs its time and the image it serves")
    out = {**state, "recreated_at": at, "recreated_image": image,
           "recreated_version": version, "complex_connection": None}
    out.pop("recreation_started_at", None)
    return out


#: The one purpose the store-review exemption admits: the admin's headless
#: reports pass (`fauna-reports-pass`), which lists and resolves open reports
#: and nothing else (`org-nests.md` § fauna.build → *Disposable*, ruled
#: 2026-11-01). No other purpose, and never the full admin app, is admitted by
#: an open review.
REPORTS_PASS = "headless reports pass"

def _admit_reports_pass(state: dict, domain: str, *, at: str,
                        submissions: list[dict] | None) -> dict:
    """The reports-pass exemption: admitted without spending the box's one
    complex connection, and whatever the calendar says (exemption (b) holds the
    calendar while a review is open), only while the store-review record holds
    an `in-review` submission of the version the box was re-created serving."""
    _instant(at, "the connection time")
    if state.get("recreation_started_at"):
        raise StaleBoxError(
            f"refusing the {REPORTS_PASS} to {domain}: a re-creation of it started at "
            f"{state['recreation_started_at']} and recorded no finish — the box being "
            "built is nobody's to sign in to yet.")
    if not state.get("recreated_at"):
        raise StaleBoxError(
            f"refusing the {REPORTS_PASS} to {domain}: no re-creation of it is recorded "
            "on this machine, so nothing says which version it serves.")
    served = state.get("recreated_version")
    if not served:
        raise StaleBoxError(
            f"refusing the {REPORTS_PASS} to {domain}: its re-creation at "
            f"{state['recreated_at']} recorded no served version (the image label was "
            "unreadable, or the record predates it), so no open review can be matched "
            "to it. Re-create the box first (org-nests.md § fauna.build).")
    if submissions is None:
        raise StaleBoxError(
            f"refusing the {REPORTS_PASS} to {domain}: the store-review record was not "
            "read, and the exemption holds only while it shows an open review "
            "(org-nests.md § fauna.build → Disposable).")
    if not _store_review().calendar_hold(submissions, serving=served):
        raise StaleBoxError(
            f"refusing the {REPORTS_PASS} to {domain}: the store-review record holds no "
            f"in-review submission of {served}, the version the box serves. Outside "
            "an open review the pass is a complex connection like any other — re-create "
            "the box first (org-nests.md § fauna.build → Disposable).")
    return {**state, "reports_pass": {"at": at}}


def admit_complex_connection(state: dict, *, purpose: str, at: str,
                             submissions: list[dict] | None = None) -> dict:
    """``state`` with this complex connection recorded — or a refusal.

    The one exemption: ``purpose`` :data:`REPORTS_PASS`, admitted without
    spending the connection while ``submissions`` (the store-review record's
    tables) holds an `in-review` submission of the version the box serves
    (:func:`_admit_reports_pass`). ``submissions`` changes nothing for any
    other purpose.

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
    if purpose == REPORTS_PASS:
        return _admit_reports_pass(state, domain, at=at, submissions=submissions)
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


def post_claim_relaunch_owed(start: str, *, relaunched: bool) -> bool:
    """Whether a drive whose post-claim session never reached Connected
    relaunches the app and signs in as the box's admin, in the same run.

    Only a fresh box (``PROVISION``), and once: on 2026-11-02 the claiming
    process's session stayed Disconnected for minutes after the box's name
    resolved and its certificate was issued, while a fresh process — the sync
    agent, then a resume run — connected to the same box; failing there spends
    the box (`org-nests.md` § fauna.build → *Disposable*, rule (2)). A resume
    already IS a fresh process signing in, so a relaunch adds nothing there, and
    a second relaunch would only hide a box that genuinely does not serve."""
    return start == PROVISION and not relaunched


# ── The certificate budget: a ledger of the boxes bought ───────────────────


def _buys_in_window(state: dict, now: datetime) -> list[datetime]:
    """The recorded buys still inside :data:`CERTIFICATE_WINDOW` at ``now``,
    oldest first. An unreadable entry is refused, never read as outside."""
    buys = [_instant(b, "a recorded box buy") for b in state.get("box_buys") or []]
    return sorted(b for b in buys if now - b < CERTIFICATE_WINDOW)


def note_box_bought(state: dict, *, at: str) -> dict:
    """``state`` with a box buy at ``at`` recorded, buys beyond the window pruned.

    The drive saves the returned state BEFORE it presses "Buy and set up": a
    box bought is a certificate requested, whatever happens after, so a run
    that dies after the click has still spent its token."""
    now = _instant(at, "the buy time")
    kept = [b.strftime("%Y-%m-%dT%H:%M:%SZ") for b in _buys_in_window(state, now)]
    return {**state, "box_buys": [*kept, at]}


def certificate_budget_refusal(state: dict, *, at: str) -> str | None:
    """Why a box may not be bought at ``at`` — the window already holds
    :data:`CERTIFICATE_BUDGET` buys, named with the instant the oldest falls
    out — or None when it may."""
    buys = _buys_in_window(state, _instant(at, "the time"))
    if len(buys) < CERTIFICATE_BUDGET:
        return None
    oldest = buys[-CERTIFICATE_BUDGET]
    refill = oldest + CERTIFICATE_WINDOW
    return (f"the certificate budget is spent: {len(buys)} boxes bought within "
            f"{CERTIFICATE_WINDOW.days} days (budget {CERTIFICATE_BUDGET}, the oldest "
            f"counted at {oldest:%Y-%m-%dT%H:%M:%SZ}); the next buy is admitted from "
            f"{refill:%Y-%m-%dT%H:%M:%SZ} (org-nests.md § fauna.build → Disposable, rule 3)")


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
    record and marks the re-creation begun.

    Refused with :class:`CertificateBudgetWait` while the certificate window
    already holds :data:`CERTIFICATE_BUDGET` buys (:func:`certificate_budget_refusal`):
    a re-creation buys a box, and a box bought past the budget serves its
    self-signed floor certificate."""
    domain = (state.get("domain") or "").lower().rstrip(".")
    if domain not in FRESH_BOX_HOSTS:
        raise StagingPlanError(
            f"{domain!r} is not the acceptance nest: the calendar re-creation is "
            f"fauna.build's alone ({sorted(FRESH_BOX_HOSTS)})"
        )
    if refusal := certificate_budget_refusal(state, at=at):
        raise CertificateBudgetWait(f"refusing to re-create {domain}: {refusal}")
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


#: The TTL a record without one of its own carries (the provider's zone default).
ZONE_DEFAULT_TTL_S = 3600


def finish_teardown(state: dict, *, removed: list[dict] = (), at: str | None = None) -> dict:
    """``state`` once the old server and records are gone: no server recorded,
    so the drive's :func:`decide_start` provisions afresh.

    ``dns_clear_at`` is when every resolver that cached a removed ADDRESS
    record has let it go (``at`` + the longest A/AAAA TTL; the drive connects
    by address, so the mail and TXT records do not hold it up). Before then
    the drive's handle check can still be answered the dead box's address,
    read it as "nest unreachable" and fail before anything is bought. A rerun
    that removes nothing keeps the earlier time."""
    new = {**state, "server_id": None, "ipv4": None}
    addresses = [r for r in removed if r.get("type") in ("A", "AAAA")]
    if addresses and at:
        ttl = max(r.get("ttl") or ZONE_DEFAULT_TTL_S for r in addresses)
        clear = _instant(at, "the teardown time") + timedelta(seconds=ttl)
        new["dns_clear_at"] = clear.strftime("%Y-%m-%dT%H:%M:%SZ")
    return new


def dns_clear_wait_s(state: dict, *, at: str) -> int:
    """Seconds until :func:`finish_teardown`'s ``dns_clear_at``; 0 when none."""
    clear = state.get("dns_clear_at")
    if not clear:
        return 0
    left = (_instant(clear, "dns_clear_at") - _instant(at, "the time")).total_seconds()
    return max(0, int(left))


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


# ── The calendar: when the acceptance nest is due its re-creation ────────────
#
# `org-nests.md` § fauna.build re-creates the box weekly and at every UAT entry,
# and a missed re-creation is a red, never silence. One verdict serves both
# readers: the dev-host trigger re-creates on DUE or OVERDUE, and the daily
# merge-gate check goes red on OVERDUE or DEAD — so the trigger acts a day
# before the guard (`admit_complex_connection`) would start refusing, and the
# red means the trigger missed. A re-creation that died part-way is DEAD and
# the trigger never retries it (`org-nests.md` § fauna.build → *Disposable*,
# rule (2)): a deterministic fault re-run every tick buys a box every tick.

#: How long before the cadence runs out the trigger re-creates the box, so a
#: re-creation that takes its hour lands before the box is overdue.
RECREATION_LEAD = timedelta(days=1)

#: How long a re-creation the teardown started may run before it counts as
#: dead: the drive and the serving gate take about an hour, and a slot queue on
#: a loaded dev host can add as much again.
RECREATION_RUN_BOUND = timedelta(hours=6)

#: The verdicts of :func:`recreation_verdict`. OVERDUE and DEAD are reds; only
#: DUE and OVERDUE make the trigger re-create — DEAD waits for a mind, whose
#: re-creation run by hand is the one way it is cleared (its teardown accepts a
#: started state and voids it); HELD is a store review's window.
RECREATION_VERDICTS = ("NO-BOX", "FRESH", "RUNNING", "HELD", "DUE", "OVERDUE", "DEAD")


def _store_review():
    """`scripts/_store_review.py`, the record's single home (its calendar-hold
    predicates are the ones this verdict reads)."""
    import sys
    scripts = str(Path(__file__).resolve().parents[3] / "scripts")
    if scripts not in sys.path:
        sys.path.insert(0, scripts)
    import _store_review as sr
    return sr


def recreation_verdict(state: dict | None, *, at: str, test_image: str | None = None,
                       reviews: list[dict] | None = None) -> tuple[str, str]:
    """``(verdict, why)`` for the acceptance nest's state file at time ``at``.

    ``state`` None (no state file on this machine: no box to re-create) is
    NO-BOX. A re-creation the teardown started within
    :data:`RECREATION_RUN_BOUND` is RUNNING; one started longer ago that wrote
    no record is DEAD — never retried by the trigger, cleared only by a
    re-creation run by hand. No record at all is OVERDUE, as is a record older than
    :data:`RECREATION_CADENCE`. A record within :data:`RECREATION_LEAD` of the
    cadence — or one whose image is not the one ``test_image`` (the digest
    `:test` names now) says the UAT channel serves — is DUE: a release
    entered UAT since. Otherwise FRESH. A time it cannot read is refused
    (:class:`StagingPlanError`), never read as fresh.

    ``reviews`` is the store-review record's submissions (None: not read).
    While one is `in-review` of the version the box serves
    (``recreated_version``), a box that would be DUE or past its cadence is
    HELD — the trigger defers and the miss check is quiet — however long the
    review lasts (org-nests.md § fauna.build → *Disposable*, ruled
    2026-11-01). The digest trigger is held too: duty 2's re-creation is the
    only one a review sees (store-review.md § During a review), and duty 2 is
    a re-creation run by hand, which never reads this verdict. A running or
    dead re-creation, or a box with no recorded version, is never held. Once
    the record answers the review, a box the hold kept past its cadence is
    DUE, not OVERDUE, until :data:`RECREATION_LEAD` after the end of the
    answer's day, so the trigger re-creates it before the miss check reads a
    red.
    """
    if state is None:
        return "NO-BOX", "no fauna.build state file on this machine: there is no box to re-create"
    now = _instant(at, "the time")
    started = state.get("recreation_started_at")
    if started:
        since = now - _instant(started, "the teardown time")
        if since <= RECREATION_RUN_BOUND:
            return "RUNNING", f"a re-creation started at {started} and is within its {RECREATION_RUN_BOUND}"
        return "DEAD", (f"a re-creation started at {started} and wrote no record within "
                        f"{RECREATION_RUN_BOUND}: it died part-way, and the trigger never "
                        "retries a death — a re-creation run by hand clears it, once the "
                        "cause is read and fixed")
    recreated = state.get("recreated_at")
    if not recreated:
        return "OVERDUE", "no re-creation of the box is recorded on this machine"
    due_at = _instant(recreated, "the re-creation time") + RECREATION_CADENCE
    verdict, why = _calendar(state, now=now, recreated=recreated, due_at=due_at,
                             test_image=test_image)
    if verdict == "FRESH" or reviews is None:
        return verdict, why
    sr = _store_review()
    serving = state.get("recreated_version")
    held = sr.calendar_hold(reviews, serving=serving)
    if held:
        open_ = ", ".join(f"{s['store']} {s['version']} (build {s['build']}, in review since "
                          f"{s['since']})" for s in held)
        return "HELD", (f"a store review of {serving}, the version the box serves, is open — "
                        f"{open_}; the calendar holds until the record answers it "
                        f"(it would be {verdict}: {why})")
    answered = sr.review_answered_on(reviews, serving=serving)
    if verdict == "OVERDUE" and answered is not None:
        answer_end = datetime(answered.year, answered.month, answered.day,
                              tzinfo=timezone.utc) + timedelta(days=1)
        if due_at <= answer_end and now <= answer_end + RECREATION_LEAD:
            return "DUE", (f"a store review of {serving} was answered on {answered}, ending the "
                           f"hold that kept the box past its cadence (due "
                           f"{due_at:%Y-%m-%d %H:%MZ}); the trigger re-creates it")
    return verdict, why


def _calendar(state: dict, *, now: datetime, recreated: str, due_at: datetime,
              test_image: str | None) -> tuple[str, str]:
    """:func:`recreation_verdict` for a recorded box, before any review hold."""
    if now > due_at:
        return "OVERDUE", (f"the last re-creation, at {recreated}, is older than the "
                           f"{RECREATION_CADENCE.days}-day cadence (due {due_at:%Y-%m-%d %H:%MZ})")
    if test_image and test_image != state.get("recreated_image"):
        return "DUE", (f"`:test` names {test_image}, not the {state.get('recreated_image')} "
                       f"the box was re-created on at {recreated}: a release entered UAT since")
    if now >= due_at - RECREATION_LEAD:
        return "DUE", f"the last re-creation, at {recreated}, falls due {due_at:%Y-%m-%d %H:%MZ}"
    return "FRESH", f"re-created at {recreated}, due {due_at:%Y-%m-%d %H:%MZ}"
