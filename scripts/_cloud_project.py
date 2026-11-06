"""Which cloud project a host's token comes from, per purpose — one map, one place.

WHY THIS EXISTS: the cloud provider scopes an API token to a whole project —
every server, firewall and DNS zone in it, nothing finer — so the
organisation's boxes and zones are cut into four projects by who holds each
token (docs/goal/architecture/deployment-threat-model.md § Concern 2, path 5;
docs/goal/architecture/org-nests.md § One zone, one project):

  * **zone** holds the organisation's own zone `fauna.build` (a registered
    domain: the provider's zones are registrable domains only) and nothing
    else. Its token is the DNS credential: the admin apps' managed DNS and
    DNS-01 renewal, and the DNS half of the provisioning drive;
  * **uat** holds the dev. and test. nests with their firewalls, and the live
    suite's throwaway boxes;
  * **acceptance** holds the acceptance nest at the `fauna.build` apex and its
    firewall. Its token lives on the dev host alone and is never placed in CI;
  * **production** holds the fauna.social apex zone and the front-door box.
    Its token lives on the dev host alone and is never placed in CI.

So a host has two projects, one per PURPOSE: its *cloud* project (the server
and its firewall — the firewall gate, the firewall copy, the per-run
admission, a box's provisioning) and its *dns* project (the zone its records
are published in — managed DNS, DNS-01). Under fauna.build they differ: every
name's dns project is `zone`; the cloud project is `acceptance` for the apex
alone and `uat` for every name below it. A name below the apex is never the
acceptance box, so a throwaway box can never be made with the acceptance
token; a fleet script names the acceptance box by the apex itself.

Every fleet script and the live test harness that speaks to the provider
resolves its token here, per host and purpose, so a run against a uat nest
can never pick up the acceptance or production token, nor a DNS write a
token that reaches a server.

THE RESOLUTION ORDER (`token_for`):
  1. `$HETZNER_API_TOKEN` when set and non-blank — how CI hands a job its one
     token; it wins everywhere, for both purposes, so a job holds exactly the
     token it was given (one project's, by construction).
  2. `~/.config/fauna/cloud/<project>.token` — the dev host's per-project
     file, mode 0600, first non-empty line. A missing file is no token: a
     purpose never falls back to another project's file.

A host outside the map resolves to no project and no file: only the env can
supply its token (a deployment that is not the organisation's).

Dependencies: Python stdlib only. Unit-tested by
tests/e2e-unified/tests/test_cloud_project.py.
"""

from __future__ import annotations

import os
import stat
from collections.abc import Mapping

ENV_VAR = "HETZNER_API_TOKEN"

ZONE = "zone"
UAT = "uat"
ACCEPTANCE = "acceptance"
PRODUCTION = "production"
PROJECTS = (ZONE, UAT, ACCEPTANCE, PRODUCTION)

#: A host's server and firewall.
CLOUD = "cloud"
#: The zone a host's records are published in.
DNS = "dns"
PURPOSES = (CLOUD, DNS)

#: The organisation's own zone, a registered domain, in the zone project; its
#: apex is the acceptance box, every name below it a uat box.
ORG_ZONE = "fauna.build"
#: The apex zone, in the production project.
PRODUCTION_APEX = "fauna.social"

#: The project a run that names no box works in, per purpose: the live
#: suite's throwaway boxes are made in the uat project, their records go in
#: the organisation's zone — never production, never the acceptance box.
DEFAULT_PROJECT = {CLOUD: UAT, DNS: ZONE}

TOKEN_DIR = "~/.config/fauna/cloud"


def normalize_host(host: str) -> str:
    """An ssh target, a URL or a bare name → the lower-case host name
    (`https://Dev.fauna.build:443/x` → `dev.fauna.build`)."""
    h = host.strip()
    if "://" in h:
        h = h.split("://", 1)[1]
    h = h.split("/", 1)[0]
    if "@" in h:
        h = h.rsplit("@", 1)[1]
    if h.startswith("["):  # a bracketed IPv6 literal keeps its colons
        return h
    return h.split(":", 1)[0].rstrip(".").lower()


def _within(host: str, zone: str) -> bool:
    return host == zone or host.endswith("." + zone)


def project_for(host: str | None, purpose: str = CLOUD) -> str | None:
    """The project host's `purpose` token comes from, or None for a host
    outside the organisation's map.

    No host (`None` or blank) is the live suite's default project."""
    if purpose not in PURPOSES:
        raise ValueError(f"unknown purpose {purpose!r} (known: {', '.join(PURPOSES)})")
    if host is None or not host.strip():
        return DEFAULT_PROJECT[purpose]
    h = normalize_host(host)
    # The zones are disjoint domains; the organisation's merely comes first.
    if _within(h, ORG_ZONE):
        if purpose == DNS:
            return ZONE
        return ACCEPTANCE if h == ORG_ZONE else UAT
    if _within(h, PRODUCTION_APEX):
        return PRODUCTION
    return None


def token_file(project: str) -> str:
    """The dev host's token file for `project`, unexpanded (`~/…`)."""
    if project not in PROJECTS:
        raise ValueError(f"unknown cloud project {project!r} (known: {', '.join(PROJECTS)})")
    return f"{TOKEN_DIR}/{project}.token"


class TokenFileModeError(RuntimeError):
    """A token file other users can read — refused rather than used."""


def _read_first_line(path: str) -> str:
    full = os.path.expanduser(path)
    try:
        st = os.stat(full)
    except OSError:
        return ""
    # POSIX only: Windows reports synthetic mode bits.
    if os.name != "nt" and st.st_mode & (stat.S_IRWXG | stat.S_IRWXO):
        raise TokenFileModeError(
            f"{full} is readable by other users (mode {stat.S_IMODE(st.st_mode):o}) — "
            f"run: chmod 600 {full}"
        )
    try:
        with open(full, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    return line
    except OSError:
        pass
    return ""


def token_for(host: str | None, env: Mapping[str, str] | None = None,
              purpose: str = CLOUD) -> str:
    """The API token of host's `purpose` project, or "" when there is none.

    Never prints or logs the value. Raises TokenFileModeError for a token file
    other users can read."""
    env = os.environ if env is None else env
    tok = env.get(ENV_VAR, "").strip()
    if tok:
        return tok
    project = project_for(host, purpose)
    if project is None:
        return ""
    return _read_first_line(token_file(project))


def where_to_put(host: str | None, purpose: str = CLOUD) -> str:
    """One line for an error message: where host's `purpose` token is looked for."""
    project = project_for(host, purpose)
    if project is None:
        return (f"{normalize_host(host or '')} is not one of the organisation's boxes — "
                f"set {ENV_VAR} to its project's token")
    return (f"set {ENV_VAR}, or write the {project} project's token to "
            f"{token_file(project)} (mode 0600)")


# ── The admin app's own DNS credential, one per nest ─────────────────────────
#
# The credential a nest's admin app keeps for managed DNS rests sealed on the
# box and in cleartext on every admin device, so it is a zone-project token
# minted for that nest's admin alone, never one of the fleet's token files
# (docs/goal/architecture/org-nests.md § One zone, one project; the threat
# model's token-per-consumer rule). Revoking it on a doubtful admin device then
# costs nothing fleet-side. Only the provider's console mints one; the dev host
# keeps it at `app_dns_token_file(host)`. There is no env override: the env
# carries a fleet token, which is exactly what may not reach an app.

#: Where the dev host keeps each nest's admin-app DNS token, one file per nest.
APP_DNS_TOKEN_DIR = f"{TOKEN_DIR}/app-dns"


class AppTokenReuseError(RuntimeError):
    """A nest's admin-app DNS token that is one of the fleet's own tokens."""


def app_dns_token_file(host: str) -> str:
    """The dev host's file for host's admin-app DNS token, unexpanded."""
    h = normalize_host(host)
    if not _within(h, ORG_ZONE):
        raise ValueError(f"{h!r} is not one of the organisation's nests under {ORG_ZONE}")
    return f"{APP_DNS_TOKEN_DIR}/{h}.token"


def app_dns_token_for(host: str, env: Mapping[str, str] | None = None) -> str:
    """The zone-project token host's admin app keeps, or "" when none is kept.

    Raises AppTokenReuseError when it equals `$HETZNER_API_TOKEN` or any of the
    fleet's project token files, and TokenFileModeError for a file other users
    can read. Never prints or logs the value."""
    env = os.environ if env is None else env
    try:
        path = app_dns_token_file(host)
    except ValueError:
        return ""
    tok = _read_first_line(path)
    if not tok:
        return ""
    fleet = {f"${ENV_VAR}": env.get(ENV_VAR, "").strip()}
    fleet.update({token_file(p): _read_first_line(token_file(p)) for p in PROJECTS})
    for where, value in fleet.items():
        if value and value == tok:
            raise AppTokenReuseError(
                f"{os.path.expanduser(path)} holds the same token as {where}: the admin "
                f"app of {normalize_host(host)} keeps a zone-project token of its own, "
                "minted for that nest alone")
    return tok


def app_dns_where_to_put(host: str) -> str:
    """One line for an error message: where host's admin-app DNS token goes."""
    return (f"mint a Read & Write token in the zone project, named for "
            f"{normalize_host(host)}'s admin, and write it to {app_dns_token_file(host)} "
            "(mode 0600) — never a copy of a fleet token file")
