"""The release's own app — which private commit an organisation drive against a
kept box builds its client from, and the build.

Owner: `docs/goal/architecture/org-nests.md` § fauna.build → *Disposable*, the
2026-11-01 ruling, rule (1): every organisation operation on one of its nests
from a dev host — the provisioning drive of the three kept boxes, the store
reviewer journey on the two staging boxes, the headless reports pass — runs a
client built from the private commit the box's image was built from, never from
the tip. Inside 0.1.x the tip owes the release no compatibility, so the tip's
app against a release's nest is a skew the drive must never buy a server on.

The resolution, every step of which either answers or REFUSES with its reason
(never a guess, never the tip, never the pin):

1. The image's commit. For a drive that is about to BUY a box, the registry's
   revision label on the channel's tag (`image_commit`, read anonymously over
   the OCI distribution API with the standard library — pull token, the tag's
   manifest or index, each platform's config blob — every platform agreeing;
   no Docker, so it answers on every dev machine). For a box already serving, the
   `commit` its `/api/v1/health` reports — a plain HTTPS probe, which the
   before-a-complex-connection rule leaves free (`health_commit`).
2. The private commit (`private_commit_for`). A private `:dev` build's label is
   the private commit itself, so a revision this repository has is taken as
   is. A public build's label is a public commit, paired to its private source
   by the `Sync-Point:` trailer the publish tool writes
   (`helpers/feature_ledger.py::sync_point_of`, read in the standing mirror;
   a commit the local clone lacks — macOS and Windows keep a clone nothing
   fetches — is read from the public repository's API, `public_sync_point`),
   resolved here to the full sha and cross-checked against the release table's
   row whose `public_commit` it is, when one exists (`release-table.md`
   § Tag gate and release table, *Promotion copies the tested digest*).
3. The build: `helpers/prev_build.py`'s commit-keyed cache, client alone
   (`nest=False`), so one release costs one build. The pin is not consulted.

Every string a box or the registry returns (the health JSON, the image labels)
is untrusted data: only a hex commit and a dotted-numeric version are taken
from it, validated before use.

CLI (what the justfile recipes and the conftest prebuild call):

    release_app.py resolve --image <ref>              print the private commit
    release_app.py resolve --box <host>
    release_app.py resolve --version <x.y.z|stable|test|dev>
    release_app.py path    (--image <ref>|--box <host>|--commit <sha>) --client <c>
    release_app.py build   (--image <ref>|--box <host>|--commit <sha>) --client <c>

`path` exits 1 when the cache is cold; `build` builds it (take a build slot
first — `just e2e-release-app-build` does). A refusal exits 3 with its reason.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import urllib.request
from dataclasses import dataclass
from pathlib import Path

if __package__ in (None, ""):  # run as a script: make `helpers.*` importable
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import feature_ledger, prev_build  # noqa: E402

REPO = Path(__file__).resolve().parents[3]
REVISION_LABEL = "org.opencontainers.image.revision"
VERSION_LABEL = "org.opencontainers.image.version"
_HEX = re.compile(r"^[0-9a-f]{7,40}$")


class ReleaseAppRefused(RuntimeError):
    """The release's app cannot be resolved or built — the reason is the message.

    Raised before anything is bought or connected; a caller never falls back to
    the tip or the pin on it."""


@dataclass(frozen=True)
class Resolution:
    """The answer: the private commit to build, and how it was reached."""

    commit: str            # the full private sha the client is built from
    revision: str          # what the image (or the box) names — public or private
    version: str | None    # the image's version label, when read off the registry
    via: str               # "private" (the revision is ours) or "sync-point"
    image: str | None = None  # the image ref it was read off, when one was

    def describe(self) -> str:
        how = ("the revision is a private commit" if self.via == "private"
               else f"public {self.revision[:12]} → Sync-Point")
        ver = f" (version {self.version})" if self.version else ""
        return f"{self.commit[:12]}{ver} — {how}"


_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
_VERSION = re.compile(r"^\d+\.\d+\.\d+$")
#: The manifest kinds a tag may name: an index (one manifest per platform) or a
#: single platform's manifest, in their OCI and Docker spellings.
_ACCEPT = ", ".join((
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
))
#: The most a registry reply is read — a manifest or config is a few KB.
_MAX_BODY = 4 * 1024 * 1024


def _fetch(url: str, headers: dict[str, str]) -> bytes:
    """GET `url` (https only) — the registry seam; an HTTP error raises."""
    if not url.startswith("https://"):
        raise ReleaseAppRefused(f"refusing a non-https registry read: {url[:64]!r}")
    req = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read(_MAX_BODY)


def _split_ref(image_ref: str) -> tuple[str, str, str]:
    """`(registry, repository, tag)` of `<registry>/<repository>:<tag>`."""
    name, _, tag = image_ref.rpartition(":")
    registry, _, repository = name.partition("/")
    if not (registry and repository and tag) or "." not in registry or "/" in tag:
        raise ReleaseAppRefused(
            f"not a <registry>/<repository>:<tag> reference: {image_ref[:80]!r}")
    return registry, repository, tag


def _image_labels(image_ref: str, fetch) -> list[dict]:
    """Every platform's config `Labels` of `image_ref`, read anonymously over
    the OCI distribution API — the reads a box's unauthenticated pull makes:
    the pull token, the tag's manifest (an index steps to each platform's
    manifest, buildx's attestation entries skipped), each manifest's config
    blob. One path on every machine: no Docker (mac and win have none)."""
    registry, repository, tag = _split_ref(image_ref)
    base = f"https://{registry}/v2/{repository}"
    token = json.loads(fetch(
        f"https://{registry}/token?scope=repository:{repository}:pull&service={registry}",
        {}))["token"]
    auth = {"Authorization": f"Bearer {token}"}

    def get_json(url: str, accept: str | None = None) -> dict:
        headers = dict(auth, **({"Accept": accept} if accept else {}))
        doc = json.loads(fetch(url, headers))
        if not isinstance(doc, dict):
            raise ValueError("the reply is not a JSON object")
        return doc

    def digest_of(node) -> str:
        digest = node.get("digest") if isinstance(node, dict) else None
        if not isinstance(digest, str) or not _DIGEST.match(digest):
            raise ValueError(f"not a sha256 digest: {str(digest)[:80]!r}")
        return digest

    top = get_json(f"{base}/manifests/{tag}", _ACCEPT)
    if "manifests" in top:  # an index: one manifest per platform
        manifests = []
        for entry in top["manifests"]:
            plat = (entry.get("platform") or {}) if isinstance(entry, dict) else {}
            if "unknown" in (plat.get("os"), plat.get("architecture")):
                continue  # an attestation manifest — it carries no image config
            manifests.append(get_json(f"{base}/manifests/{digest_of(entry)}", _ACCEPT))
    else:
        manifests = [top]
    labels = []
    for manifest in manifests:
        config = get_json(f"{base}/blobs/{digest_of(manifest.get('config'))}")
        found = (config.get("config") or {}).get("Labels") or {}
        if not isinstance(found, dict):
            raise ValueError("the image config's Labels is not a map")
        labels.append(found)
    return labels


def image_commit(image_ref: str, fetch=_fetch) -> tuple[str, str | None]:
    """`(revision, version)` — the labels `image_ref` carries on the registry.

    `fetch(url, headers)` returns a reply's body and raises on an HTTP error.
    Every platform of a multi-platform image must agree on the revision; a
    missing image, an unreachable registry, an unreadable or missing label, or
    a disagreement is a refusal, never a traceback. The registry's text is
    untrusted data: only a hex revision and a dotted-numeric version are taken
    from it (any other version label reads as none)."""
    try:
        labels = _image_labels(image_ref, fetch)
    except ReleaseAppRefused:
        raise
    except Exception as exc:  # network, HTTP, JSON, shape — all one answer
        raise ReleaseAppRefused(
            f"cannot read {image_ref} off the registry "
            f"({type(exc).__name__}: {str(exc)[:160]})") from None
    revisions = {str(lab.get(REVISION_LABEL) or "").strip().lower() for lab in labels}
    if not labels or "" in revisions:
        raise ReleaseAppRefused(f"{image_ref} carries no {REVISION_LABEL} label")
    if len(revisions) != 1:
        raise ReleaseAppRefused(
            f"{image_ref}'s platforms disagree on {REVISION_LABEL}: "
            f"{sorted(r[:16] for r in revisions)}")
    revision = revisions.pop()
    if not _HEX.match(revision):
        raise ReleaseAppRefused(
            f"{image_ref}'s {REVISION_LABEL} is not a commit ({revision[:16]!r})")
    versions = {str(lab.get(VERSION_LABEL) or "").strip() for lab in labels}
    version = versions.pop() if len(versions) == 1 else None
    return revision, (version if version and _VERSION.match(version) else None)


def health_commit(host: str, fetch=None) -> str:
    """The commit `https://<host>/api/v1/health` reports — a plain HTTPS probe.

    Box-sourced: only a hex commit is taken from the reply, anything else is a
    refusal (`dev`, the unstamped build's answer, included)."""
    url = f"https://{host}/api/v1/health"
    try:
        if fetch is not None:
            body = fetch(url)
        else:
            with urllib.request.urlopen(url, timeout=30) as r:  # https only
                body = r.read().decode("utf-8", "replace")
        commit = str(json.loads(body).get("commit") or "").strip().lower()
    except Exception as exc:  # network, TLS, JSON — all one answer: unresolved
        raise ReleaseAppRefused(f"cannot read the build commit off {url} ({exc})") from None
    if not _HEX.match(commit):
        raise ReleaseAppRefused(
            f"{url} reports no commit-stamped build (commit={commit[:16]!r})")
    return commit


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True,
                          text=True, timeout=30, check=False)


def _full_sha(rev: str, repo: Path) -> str | None:
    out = _git(repo, "rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}")
    return out.stdout.strip() if out.returncode == 0 and out.stdout.strip() else None


PUBLIC_REPO = "faunasocial/fauna"


def public_sync_point(public_commit: str, public_clone: Path | None = None,
                      repo: Path = REPO, *, read_clone=None,
                      fetch=None) -> tuple:
    """`(private short sha, reason)` — the `Sync-Point:` trailer of a public commit.

    Read first in the public clone (`feature_ledger.sync_point_of`; on Linux the
    standing mirror holds every public commit, since it pushes them). The clone
    on macOS and Windows is a transform-written build fixture nothing fetches, so a
    commit it lacks is read from the public repository itself: the commit's
    message over GitHub's REST API, anonymously. Nothing is written to the clone
    (`publish-gates.md` § The standing mirror: only `fauna-publish` writes it).
    The trailer stays the authority either way; the reply is untrusted data, of
    which only the echoed sha and the trailer's hex are taken."""
    read_clone = read_clone or feature_ledger.sync_point_of
    short, why = read_clone(public_commit, public_clone=public_clone, repo=repo)
    if short is not None:
        return short, ""
    fetch = fetch or _fetch
    url = f"https://api.github.com/repos/{PUBLIC_REPO}/commits/{public_commit}"
    try:
        reply = json.loads(fetch(url, {"Accept": "application/vnd.github+json",
                                       "User-Agent": "fauna-release-app"}))
        sha = str(reply.get("sha") or "").lower()
        message = str((reply.get("commit") or {}).get("message") or "")
    except Exception as exc:  # noqa: BLE001 — any failure is a stated refusal
        return None, (f"{why}; reading it from {PUBLIC_REPO} failed too "
                      f"({type(exc).__name__}: {str(exc)[:120]})")
    if not (_HEX.match(sha) and sha.startswith(public_commit.lower())):
        return None, (f"{why}; {PUBLIC_REPO} answered for another commit "
                      f"({sha[:12] or '?'})")
    return feature_ledger.sync_point_in_message(public_commit, message)


def private_commit_for(revision: str, *, repo: Path = REPO,
                       public_clone: Path | None = None,
                       releases: list | None = None,
                       sync_point_of=public_sync_point) -> tuple[str, str]:
    """`(full private sha, via)` for the commit an image or a box names.

    `via` is ``"private"`` when this repository has the revision itself (a
    private `:dev` build), else ``"sync-point"`` (a public build, paired by its
    trailer). `releases` (default: the release table) cross-checks a public
    revision's pairing; a row that pairs it to another commit is a refusal."""
    revision = (revision or "").strip().lower()
    if not _HEX.match(revision):
        raise ReleaseAppRefused(f"{revision[:16]!r} is not a commit")
    full = _full_sha(revision, repo)
    if full:
        return full, "private"
    short, why = sync_point_of(revision, public_clone=public_clone, repo=repo)
    if short is None:
        raise ReleaseAppRefused(
            f"{revision[:12]} is not a private commit and has no readable pairing: {why}")
    full = _full_sha(short, repo)
    if not full:
        raise ReleaseAppRefused(
            f"public {revision[:12]}'s Sync-Point {short} is not a commit in this "
            "repository (fetch origin)")
    if releases is None:
        releases = feature_ledger._load_releases(feature_ledger.LEDGER / "releases.json")
    for row in releases:
        pub = str(row.get("public_commit") or "").lower()
        if pub and (pub.startswith(revision) or revision.startswith(pub)):
            recorded = str(row.get("commit") or "").lower()
            if recorded != full:
                raise ReleaseAppRefused(
                    f"public {revision[:12]} pairs to {full[:12]} by its Sync-Point but "
                    f"the release table records {recorded[:12] or '?'} "
                    f"(version {row.get('version')}) — refusing to choose")
    return full, "sync-point"


def resolve_image(image_ref: str, *, fetch=_fetch, **kw) -> Resolution:
    """The release app's commit for a box about to be built on `image_ref`."""
    revision, version = image_commit(image_ref, fetch=fetch)
    commit, via = private_commit_for(revision, **kw)
    return Resolution(commit=commit, revision=revision, version=version, via=via,
                      image=image_ref)


IMAGE_REPO = "ghcr.io/faunasocial/nest"
# The update channels a box is built on (`image-pipeline.md` § Image tags &
# channels), each to its moving tag; the order is the search order for a version
# no `:<version>` tag carries yet (a release candidate rides `:test` first).
CHANNEL_TAGS = {"stable": "latest", "test": "test", "dev": "dev"}


def resolve_version(name: str, *, fetch=_fetch, **kw) -> Resolution:
    """The private commit of a product VERSION (`0.1.7`) or a CHANNEL (`test`).

    A channel reads its moving tag. A version reads its own `:<version>` tag —
    which the promote writes only at release — and, while that is absent, the
    channel whose image's version label is that version. Never the tip: a name
    nothing on the registry carries is a refusal naming what each channel serves.
    """
    if name in CHANNEL_TAGS:
        return resolve_image(f"{IMAGE_REPO}:{CHANNEL_TAGS[name]}", fetch=fetch, **kw)
    if not _VERSION.match(name or ""):
        raise ReleaseAppRefused(
            f"{name[:32]!r} is neither a version (like 0.1.7) nor a channel "
            f"({', '.join(CHANNEL_TAGS)})")
    try:
        revision, version = image_commit(f"{IMAGE_REPO}:{name}", fetch=fetch)
    except ReleaseAppRefused:
        revision = None
    if revision is not None:
        if version != name:
            raise ReleaseAppRefused(
                f"{IMAGE_REPO}:{name} is labelled version {version!r}, not {name}")
        commit, via = private_commit_for(revision, **kw)
        return Resolution(commit=commit, revision=revision, version=version, via=via,
                          image=f"{IMAGE_REPO}:{name}")
    served = []
    for channel, tag in CHANNEL_TAGS.items():
        try:
            revision, version = image_commit(f"{IMAGE_REPO}:{tag}", fetch=fetch)
        except ReleaseAppRefused:
            continue
        if version == name:
            commit, via = private_commit_for(revision, **kw)
            return Resolution(commit=commit, revision=revision, version=version, via=via,
                              image=f"{IMAGE_REPO}:{tag}")
        served.append(f"{channel} {version or '(unlabelled)'}")
    raise ReleaseAppRefused(
        f"no image carries version {name} — the channels serve: "
        f"{', '.join(served) or 'nothing readable'}")


def resolve_box(host: str, *, fetch=None, **kw) -> Resolution:
    """The release app's commit for the box `host` is serving now."""
    revision = health_commit(host, fetch=fetch)
    commit, via = private_commit_for(revision, **kw)
    return Resolution(commit=commit, revision=revision, version=None, via=via)


def commit_matches(served: str, revision: str) -> bool:
    """Whether a box's health `commit` names the same commit as `revision`
    (either may be a short prefix of the other)."""
    a, b = (served or "").strip().lower(), (revision or "").strip().lower()
    return bool(a) and bool(b) and (a.startswith(b) or b.startswith(a))


def binary_stamp(binary: Path, commit: str) -> bool:
    """Whether `binary` carries `commit`'s build-commit stamp.

    `prev_build` stamps its builds with `FAUNA_BUILD_COMMIT=<commit[:8]>`, which
    the artifact's `commit!()` expansion bakes into the binary as a string — so
    the bytes themselves say which commit the app is, independent of the cache
    key that names its directory. False for a build whose tree stamps nothing."""
    try:
        return commit[:8].encode() in Path(binary).read_bytes()
    except OSError:
        return False


#: What the conftest override launched this session, per app:
#: {app: {"commit", "revision", "path"}} — the record the pre-buy check reads.
_LAUNCHED: dict[str, dict] = {}


def note_launched(app: str, commit: str, revision: str, path: str) -> None:
    _LAUNCHED[app] = {"commit": commit, "revision": revision, "path": path}


def launched(app: str | None = None) -> dict | None:
    """The record for `app`, or — unnamed — the session's one launched release
    build (the native apps are session-scoped, so a run launches one)."""
    if app is not None:
        return _LAUNCHED.get(app)
    return next(iter(_LAUNCHED.values())) if len(_LAUNCHED) == 1 else None


#: The catalog the apps render from (the harness's `i18n.strings.S` is the
#: tip's generation of it).
STRINGS_PATH = "i18n/strings/en.yaml"


def _show(commit: str, path: str) -> str | None:
    out = _git(REPO, "show", f"{commit}:{path}")
    return out.stdout if out.returncode == 0 else None


def app_string(key: str, app: str | None = None, *, show=_show) -> str | None:
    """The English string `key` (dotted, as in `S`: ``error.sync.x``) as the
    launched release build renders it — read from that build's own commit's
    catalog — or ``None`` when no release build was launched or the commit has
    no such key. A classifier matching an app-rendered string against the
    tip's `S` misreads a release app whose wording has moved since (0.1.5's
    device-cap notice said "plan" where the tip says "tier": every resume of
    a box at the cap timed out unrecognized, 2026-11-09)."""
    rec = launched(app)
    if not rec:
        return None
    text = show(rec["commit"], STRINGS_PATH)
    if not text:
        return None
    import yaml
    node = yaml.safe_load(text)
    for part in key.split("."):
        if not isinstance(node, dict) or part not in node:
            return None
        node = node[part]
    return node if isinstance(node, str) else None


def prebuy_refusal(expected: Resolution, app: str | None = None) -> str | None:
    """Why the app this session launched is NOT `expected`'s commit, or None.

    The drive's assertion before "Buy and set up": `expected` is resolved
    afresh off the registry just before the buy, so a tag that moved since the
    session launched its app is caught too. The launched record is the
    override's (the commit the cache was keyed on); the binary's own bytes are
    checked for that commit's build stamp as the independent witness, and a
    build whose tree stamps nothing is said so rather than passed silently."""
    rec = launched(app)
    if rec is None:
        return (f"no release build of {app or 'the app'} was launched this session "
                "(the `release_app` marker did not reach the app fixture)")
    if rec["commit"] != expected.commit:
        return (f"the launched app is {rec['commit'][:12]} but the image now names "
                f"{expected.describe()}")
    if not binary_stamp(Path(rec["path"]), expected.commit):
        print(f"[release_app] note: {rec['path']} carries no {expected.commit[:8]} build "
              "stamp; the cache key stands as the record", flush=True)
    return None


def cached(commit: str, client: str) -> dict[str, Path] | None:
    return prev_build.cached_binaries(commit, "debug", client, nest=False)


def absent_reason(commit: str, client: str) -> str | None:
    return prev_build.client_absent_reason(commit, client)


def build(commit: str, client: str) -> dict[str, Path]:
    """Build (or find cached) the client at `commit`. Take a build slot first."""
    absent = absent_reason(commit, client)
    if absent:
        raise ReleaseAppRefused(absent)
    return prev_build.build_prev(commit, "debug", client, nest=False)


# ── CLI ─────────────────────────────────────────────────────────────────────

def _target(args: list[str]) -> Resolution:
    if "--commit" in args:
        commit = args[args.index("--commit") + 1].lower()
        full = _full_sha(commit, REPO) if _HEX.match(commit) else None
        if not full:
            raise ReleaseAppRefused(f"{commit[:16]!r} is not a commit in this repository")
        return Resolution(commit=full, revision=full, version=None, via="private")
    if "--image" in args:
        return resolve_image(args[args.index("--image") + 1])
    if "--box" in args:
        return resolve_box(args[args.index("--box") + 1])
    if "--version" in args:
        return resolve_version(args[args.index("--version") + 1])
    raise SystemExit("release_app.py: name the target — --image <ref>, --box <host>, "
                     "--version <x.y.z|stable|test|dev> or --commit <sha>")


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args or args[0] in ("-h", "--help") or args[0] not in ("resolve", "path", "build"):
        print(__doc__.split("CLI (", 1)[1].split("\n", 1)[1], end="")
        return 0 if args and args[0] in ("-h", "--help") else 2
    verb = args[0]
    try:
        res = _target(args)
        print(f"[release_app] {res.describe()}", file=sys.stderr)
        if verb == "resolve":
            print(res.commit)
            return 0
        if "--client" not in args:
            raise SystemExit("release_app.py: --client <tui|linux|reports-pass> is required")
        client = args[args.index("--client") + 1]
        absent = absent_reason(res.commit, client)
        if absent:
            raise ReleaseAppRefused(absent)
        if verb == "path":
            found = cached(res.commit, client)
            if not found:
                print(f"[release_app] {client} at {res.commit[:12]} is not cached",
                      file=sys.stderr)
                return 1
        else:
            found = build(res.commit, client)
        print(f"commit={res.commit}")
        for key, path in found.items():
            print(f"{key}={path}")
        return 0
    except ReleaseAppRefused as exc:
        print(f"REFUSED: {exc}", file=sys.stderr)
        return 3


if __name__ == "__main__":
    sys.exit(main())
