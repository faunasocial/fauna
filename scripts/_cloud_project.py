"""Which cloud project a box lives in, and that project's API token — one map, one place.

WHY THIS EXISTS: the cloud provider scopes an API token to a whole project —
every server, firewall and DNS zone in it, nothing finer — so the
organisation's boxes are split across projects by what a token reaches
(docs/goal/architecture/deployment-threat-model.md § Concern 2, path 5;
docs/goal/architecture/org-nests.md § One zone, one project):

  * the **uat** project holds the organisation's own zone `fauna.build` (a
    registered domain: the provider's zones are registrable domains only) and
    the three nests in it (dev., test. and the acceptance nest at the apex), with
    their firewalls; the live suite's throwaway boxes are made there too;
  * the **production** project holds the fauna.social apex zone and the
    front-door box. Its token lives on the dev host alone and is never placed
    in CI.

Every fleet script and the live test harness that speaks to the provider
resolves its token here, per box, so a run against a uat nest can never pick
up the production token by accident.

THE RESOLUTION ORDER (`token_for`):
  1. `$HETZNER_API_TOKEN` when set and non-blank — how CI hands a job its one
     token; it wins everywhere, so a job holds exactly the token it was given.
  2. `~/.config/fauna/cloud/<project>.token` — the dev host's per-project
     file, mode 0600, first non-empty line.
  3. For the production project only, the legacy `~/.hetzner-token`: today's
     single project becomes the production project once the dev nest leaves
     it, so that file IS the production token. Its read goes when the dev
     nest is re-created in the uat project.

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

UAT = "uat"
PRODUCTION = "production"
PROJECTS = (UAT, PRODUCTION)

#: The organisation's own zone, a registered domain, in the uat project.
UAT_ZONE = "fauna.build"
#: The apex zone, in the production project.
PRODUCTION_APEX = "fauna.social"
#: TRANSITIONAL: the dev nest still runs as dev.example.com in today's single
#: project, which becomes the production project once it leaves. Removed when
#: the dev nest is re-created as dev.fauna.build.
_LEGACY_PRODUCTION_ZONE = "example.com"

#: The project a run that names no box works in: the live suite's throwaway
#: boxes are made in the uat project, never in production.
DEFAULT_PROJECT = UAT

TOKEN_DIR = "~/.config/fauna/cloud"
#: TRANSITIONAL: the production project's token under its pre-split name.
LEGACY_TOKEN_FILE = "~/.hetzner-token"


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


def project_for(host: str | None) -> str | None:
    """`uat`, `production`, or None for a host outside the organisation's map.

    No host (`None` or blank) is the live suite's default project."""
    if host is None or not host.strip():
        return DEFAULT_PROJECT
    h = normalize_host(host)
    # The two zones are disjoint domains; the uat check merely comes first.
    if _within(h, UAT_ZONE):
        return UAT
    if _within(h, PRODUCTION_APEX) or _within(h, _LEGACY_PRODUCTION_ZONE):
        return PRODUCTION
    return None


def token_file(project: str) -> str:
    """The dev host's token file for `project`, unexpanded (`~/…`)."""
    if project not in PROJECTS:
        raise ValueError(f"unknown cloud project {project!r} (known: {', '.join(PROJECTS)})")
    return f"{TOKEN_DIR}/{project}.token"


class TokenFileModeError(RuntimeError):
    """A token file other users can read — refused rather than used."""


def _read_first_line(path: str, *, private: bool = True) -> str:
    full = os.path.expanduser(path)
    try:
        st = os.stat(full)
    except OSError:
        return ""
    # POSIX only: Windows reports synthetic mode bits. The legacy file predates
    # the rule and is read as it stands until it goes.
    if private and os.name != "nt" and st.st_mode & (stat.S_IRWXG | stat.S_IRWXO):
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


def token_for(host: str | None, env: Mapping[str, str] | None = None) -> str:
    """The API token for the project `host` lives in, or "" when there is none.

    Never prints or logs the value. Raises TokenFileModeError for a token file
    other users can read."""
    env = os.environ if env is None else env
    tok = env.get(ENV_VAR, "").strip()
    if tok:
        return tok
    project = project_for(host)
    if project is None:
        return ""
    tok = _read_first_line(token_file(project))
    if not tok and project == PRODUCTION:
        tok = _read_first_line(LEGACY_TOKEN_FILE, private=False)
    return tok


def where_to_put(host: str | None) -> str:
    """One line for an error message: where the token for `host` is looked for."""
    project = project_for(host)
    if project is None:
        return (f"{normalize_host(host or '')} is not one of the organisation's boxes — "
                f"set {ENV_VAR} to its project's token")
    files = token_file(project)
    if project == PRODUCTION:
        files += f" (or the legacy {LEGACY_TOKEN_FILE})"
    return f"set {ENV_VAR}, or write the {project} project's token to {files} (mode 0600)"
