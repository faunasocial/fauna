"""The Microsoft Store leg: submit a release's two Store packages to Partner Center.

Owner: docs/goal/architecture/installers/store-review.md § The store legs (the
shape all three legs follow) and docs/goal/architecture/installers/windows.md
§ Store distribution (the store's own mechanics). Runs as steps of the public
repository's `release.yml` `release` job, inside the `release` Environment,
after the downloads are published; the packages come from the same run's
credential-free `release-store-windows.yml` jobs.

What it does, converging (a re-run reads the store first and goes on from what
the store already holds, so it uploads nothing twice):

  1. Checks its inputs before any call, the dry run stopping here: both
     packages present and matching their SHA-256, each package's identity
     carrying this version and its own arch, each arch's certification-kit
     report re-graded against the tree's allowlist, the version's *What's new*
     text (`installer/store-texts/microsoft-store/whats-new.txt`) approved for
     this version, and how far the leg goes (`installer/store-legs.toml`).
  2. With the credential (FAUNA_MSSTORE_TENANT_ID / _CLIENT_ID / _CLIENT_SECRET,
     an Entra application in Partner Center's Manager role), reads the app:
     this version already published → records `serving`; a submission of this
     version already in flight → records where it is (a pending commit is
     finished); any other submission in flight → red, left as it is.
  3. Otherwise clones the last published submission, replaces its packages
     with these two, leaves the visibility as it is (Public only when
     `furthest = "public"`), puts the text in the listing's release notes,
     publishes as soon as certification passes, uploads, commits, waits
     (bounded) for the commit to leave CommitStarted, and records `in-review`.

It never polls certification for days and never writes
`installer/store-review.toml`: its result goes to the run artifact
`store-legs.json` (scripts/_store_legs.py), which the attempt reads.

API: Microsoft Store submission API, `https://manage.devcenter.microsoft.com/v1.0/my/`
with a client-credentials token for resource `https://manage.devcenter.microsoft.com`
(read 2026-11-12 from Microsoft's "Create and manage submissions" and "Manage app
submissions" pages, the API that serves MSIX apps). Stdlib only.
"""
from __future__ import annotations

import argparse
import base64
import dataclasses
import datetime
import hashlib
import importlib.util
import io
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path
from typing import Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _store_legs  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
STORE = "microsoft-store"
# The Store ID Partner Center assigned at name reservation
# (installers/windows.md § Identity & coexistence) — the API's applicationId.
APP_ID = "9PLWNB0DJ0V7"
ARCHES = ("x64", "arm64")
TOKEN_URL = "https://login.microsoftonline.com/{tenant}/oauth2/token"
RESOURCE = "https://manage.devcenter.microsoft.com"
API = "https://manage.devcenter.microsoft.com/v1.0/my/applications/" + APP_ID
CREDENTIAL_ENV = ("FAUNA_MSSTORE_TENANT_ID", "FAUNA_MSSTORE_CLIENT_ID", "FAUNA_MSSTORE_CLIENT_SECRET")
ALLOWLIST = Path("apps/fauna-windows/installer/store/certification-allowlist.toml")

IN_FLIGHT = ("CommitStarted", "PreProcessing", "Certification", "Release",
             "PendingPublication", "Publishing")
FAILED = ("CommitFailed", "PreProcessingFailed", "CertificationFailed", "ReleaseFailed",
          "PublishFailed", "Canceled")
COMMIT_WAIT_S = 600
COMMIT_POLL_S = 15
BLOCK = 4 * 1024 * 1024
TEXT_LOCALE = "en-us"

_MANIFEST_NS = "{http://schemas.microsoft.com/appx/manifest/foundation/windows10}"
_VERSION = re.compile(r"^\d+\.\d+\.\d+$")


class LegError(Exception):
    """The leg is red; the message says why and is safe to print."""


def _data(value: object, limit: int = 300) -> str:
    """Text the store sent, printed as data: escaped and bounded, never instructions."""
    text = json.dumps(value, ensure_ascii=True)
    return text if len(text) <= limit else text[:limit] + "...(cut)"


# ---------------------------------------------------------------- inputs


@dataclasses.dataclass
class Inputs:
    version: str
    build: str
    packages: dict[str, Path]
    text: str
    furthest: str


def package_name(version: str, arch: str) -> str:
    """The name a package carries inside the upload, version first so a pending
    submission of this version is recognisable before the store has read it."""
    return f"Fauna-Store-{version}-{arch}.msix"


def _grader():
    spec = importlib.util.spec_from_file_location(
        "grade_store_certification", Path(__file__).resolve().parent / "grade-store-certification.py")
    mod = importlib.util.module_from_spec(spec)
    sys.modules.setdefault("grade_store_certification", mod)
    spec.loader.exec_module(mod)
    return mod


def tree_version(repo: Path) -> str:
    """The product version the tree builds (`Package.wxs`, the one home a Store
    package's version is read from) — what a dry run on a branch checks against."""
    wxs = repo / "apps" / "fauna-windows" / "installer" / "Package.wxs"
    m = re.search(r'\bVersion="(\d+\.\d+\.\d+)"', wxs.read_text(encoding="utf-8")) if wxs.is_file() else None
    if not m:
        raise LegError(f"could not read the product version from {wxs.relative_to(repo)}")
    return m.group(1)


def check_inputs(version: str, packages: Path, certification: Path, repo: Path | None = None) -> Inputs:
    """Every input the leg needs, checked before any store call. `version` is
    X.Y.Z, or `tree` for the version the tree builds."""
    repo = repo or REPO
    if version == "tree":
        version = tree_version(repo)
    if not _VERSION.match(version):
        raise LegError(f"version must be X.Y.Z, got {version!r}")
    build = f"{version}.0"
    found: dict[str, Path] = {}
    grader = _grader()
    allow_path = repo / ALLOWLIST
    if not allow_path.is_file():
        raise LegError(f"{ALLOWLIST} is missing: the certification grade reads it")
    allowed = grader.parse_allowlist(allow_path.read_text(encoding="utf-8"))
    for arch in ARCHES:
        msix = packages / f"Fauna-Store-{arch}.msix"
        sums = packages / f"Fauna-Store-{arch}.msix.sha256"
        if not msix.is_file() or not sums.is_file():
            raise LegError(f"the {arch} package or its SHA-256 is missing from {packages}: "
                           "the run's build-store job did not finish green")
        want = sums.read_text(encoding="utf-8").split()[0].lower()
        got = hashlib.sha256(msix.read_bytes()).hexdigest()
        if got != want:
            raise LegError(f"{msix.name} hashes {got}, its .sha256 says {want}")
        try:
            with zipfile.ZipFile(msix) as z:
                identity = ET.fromstring(z.read("AppxManifest.xml")).find(f"{_MANIFEST_NS}Identity")
        except (zipfile.BadZipFile, KeyError, ET.ParseError) as exc:
            raise LegError(f"{msix.name} is not a readable package ({exc})") from None
        ident = (identity.get("Version"), identity.get("ProcessorArchitecture")) if identity is not None else None
        if ident != (build, arch):
            raise LegError(f"{msix.name} carries identity version/arch {ident}, this upload is {(build, arch)}")
        report = certification / f"certification-{arch}.xml"
        if not report.is_file():
            raise LegError(f"no certification-kit report for {arch} at {report}")
        try:
            _overall, tests = grader.parse_report(report.read_text(encoding="utf-8-sig"))
        except grader.GradeError as exc:
            raise LegError(f"the {arch} certification report does not read: {exc}") from None
        failures, _accepted, _warnings = grader.grade(tests, allowed, arch)
        if failures:
            raise LegError(f"the {arch} package failed the certification kit: {', '.join(failures)}")
        found[arch] = msix
    try:
        (name,) = _store_legs.LEG_TEXTS[STORE]["testing"]  # one text on every track
        text = _store_legs.read_text(STORE, name, version, repo / "installer" / "store-texts")
        furthest = _store_legs.furthest(STORE, repo / "installer" / "store-legs.toml")
    except _store_legs.LegError as exc:
        raise LegError(str(exc)) from None
    return Inputs(version, build, found, text, furthest)


def credential(env: dict[str, str]) -> tuple[str, str, str]:
    missing = [name for name in CREDENTIAL_ENV if not env.get(name)]
    if missing:
        raise LegError(f"the release Environment holds no {', '.join(missing)}: the Microsoft Store "
                       "submission credential is the owner's to enter (store-review.md § The store legs)")
    return tuple(env[name] for name in CREDENTIAL_ENV)  # type: ignore[return-value]


# ---------------------------------------------------------------- the store


Transport = Callable[[str, str, dict[str, str], bytes | None], tuple[int, bytes]]


def urllib_transport(method: str, url: str, headers: dict[str, str], body: bytes | None) -> tuple[int, bytes]:
    req = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read()


class PartnerCenter:
    def __init__(self, transport: Transport, tenant: str, client: str, secret: str):
        self._t = transport
        form = urllib.parse.urlencode({"grant_type": "client_credentials", "client_id": client,
                                       "client_secret": secret, "resource": RESOURCE}).encode()
        status, body = self._t("POST", TOKEN_URL.format(tenant=urllib.parse.quote(tenant, safe="")),
                               {"Content-Type": "application/x-www-form-urlencoded"}, form)
        token = None
        if status == 200:
            try:
                token = json.loads(body).get("access_token")
            except ValueError:
                token = None
        if not token:
            raise LegError(f"Microsoft Entra refused the credential (HTTP {status}): check the "
                           "application's key has not expired and that it holds the Manager role")
        self._auth = {"Authorization": f"Bearer {token}"}

    def call(self, method: str, path: str, payload: dict | None = None) -> dict:
        headers = dict(self._auth)
        body = None
        if payload is not None:
            headers["Content-Type"] = "application/json"
            body = json.dumps(payload).encode()
        status, raw = self._t(method, API + path, headers, body)
        if status >= 300:
            try:
                detail = json.loads(raw)
            except ValueError:
                detail = raw[:300].decode("utf-8", "replace")
            hint = ""
            if status == 404 and path == "":
                hint = (" — the API serves only an app that already has a published submission, "
                        "and the first one is the owner's Partner Center visit")
            raise LegError(f"Partner Center answered {method} {path or '/'} with HTTP {status}{hint}: "
                           f"{_data(detail)}")
        return json.loads(raw) if raw else {}

    def upload(self, sas_url: str, data: bytes) -> None:
        """The zip to the submission's blob, in blocks (a single Put Blob is capped
        at 64 MiB under the older storage versions an ingestion SAS may name)."""
        ids = []
        for n, start in enumerate(range(0, len(data), BLOCK)):
            block_id = base64.b64encode(f"{n:08d}".encode()).decode()
            ids.append(block_id)
            url = f"{sas_url}&comp=block&blockid={urllib.parse.quote(block_id, safe='')}"
            status, _ = self._t("PUT", url, {}, data[start:start + BLOCK])
            if status >= 300:
                raise LegError(f"the package upload's block {n} was refused (HTTP {status})")
        listing = "".join(f"<Latest>{i}</Latest>" for i in ids)
        body = f'<?xml version="1.0" encoding="utf-8"?><BlockList>{listing}</BlockList>'.encode()
        status, _ = self._t("PUT", f"{sas_url}&comp=blocklist", {"Content-Type": "application/xml"}, body)
        if status >= 300:
            raise LegError(f"the package upload's block list was refused (HTTP {status})")


# ---------------------------------------------------------------- the leg


def _ours(submission: dict, inputs: Inputs) -> bool:
    names = {package_name(inputs.version, a) for a in ARCHES}
    for pkg in submission.get("applicationPackages") or []:
        if pkg.get("fileStatus") == "PendingDelete":
            continue
        if pkg.get("fileName") in names or pkg.get("version") == inputs.build:
            return True
    return False


def _track(submission: dict) -> str:
    return "public" if submission.get("visibility") == "Public" else "private-audience"


def _upload_zip(inputs: Inputs) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as z:
        for arch in ARCHES:
            z.write(inputs.packages[arch], package_name(inputs.version, arch))
    return buf.getvalue()


def fill(submission: dict, inputs: Inputs) -> dict:
    """The cloned submission with this release's packages, text and publish mode."""
    sub = json.loads(json.dumps(submission))
    packages = [dict(p, fileStatus="PendingDelete") for p in sub.get("applicationPackages") or []]
    for arch in ARCHES:
        packages.append({"fileName": package_name(inputs.version, arch), "fileStatus": "PendingUpload",
                         "minimumDirectXVersion": "None", "minimumSystemRam": "None"})
    sub["applicationPackages"] = packages
    sub["targetPublishMode"] = "Immediate"
    if inputs.furthest == "public":
        sub["visibility"] = "Public"
    listings = sub.get("listings") or {}
    if TEXT_LOCALE not in listings:
        raise LegError(f"the published submission has no {TEXT_LOCALE} listing to carry the release notes")
    for locale, listing in listings.items():
        base = listing.setdefault("baseListing", {})
        # The round approves the English text only; another locale's notes from
        # an earlier version are cleared, never left describing it.
        base["releaseNotes"] = inputs.text if locale == TEXT_LOCALE else ""
    return sub


def _wait_commit(pc: PartnerCenter, sid: str, sleep: Callable[[float], None]) -> dict:
    waited = 0.0
    while True:
        status = pc.call("GET", f"/submissions/{sid}/status")
        if status.get("status") != "CommitStarted":
            return status
        if waited >= COMMIT_WAIT_S:
            raise LegError(f"submission {sid} was still CommitStarted after {COMMIT_WAIT_S} s; "
                           "a re-run of the job resumes it")
        sleep(COMMIT_POLL_S)
        waited += COMMIT_POLL_S


def _settle(pc: PartnerCenter, sid: str, status: dict, inputs: Inputs, record: Path, track: str) -> str:
    state = status.get("status")
    if state in FAILED:
        details = status.get("statusDetails") or {}
        raise LegError(f"submission {sid} is {state}: {_data(details.get('errors') or details)}")
    if state == "Published":
        return _record(record, inputs, track, "serving", sid)
    if state in IN_FLIGHT:
        return _record(record, inputs, track, "in-review", sid)
    raise LegError(f"submission {sid} reads status {_data(state)}, which this leg does not know")


def _record(record: Path, inputs: Inputs, track: str, state: str, sid: str) -> str:
    # `read` is the minute of the store's own reading this state came from: the
    # status call just made, so now.
    read = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%MZ")
    leg = {"store": STORE, "version": inputs.version, "build": inputs.build, "track": track,
           "state": state, "read": read, "ids": {"application": APP_ID, "submission": sid},
           "note": f"Partner Center submission {sid}"}
    try:
        _store_legs.add_leg(record, leg)
    except _store_legs.LegError as exc:
        raise LegError(f"the store-legs record refused this leg: {exc}") from None
    print(f"MICROSOFT STORE: {inputs.version} {state} on {track} (submission {sid})")
    return state


def submit(pc: PartnerCenter, inputs: Inputs, record: Path,
           sleep: Callable[[float], None] = time.sleep) -> str:
    """Take `inputs` to the furthest state this leg reaches, converging on what the
    store already holds; returns the state recorded."""
    app = pc.call("GET", "")
    last = (app.get("lastPublishedApplicationSubmission") or {}).get("id")
    if last:
        published = pc.call("GET", f"/submissions/{last}")
        if _ours(published, inputs):
            return _record(record, inputs, _track(published), "serving", str(last))
    elif not app.get("pendingApplicationSubmission"):
        raise LegError("the app has no published submission: the first one is the owner's Partner "
                       "Center visit (store-review.md § The store legs, what stays a human's)")
    pending = (app.get("pendingApplicationSubmission") or {}).get("id")
    if pending:
        sub = pc.call("GET", f"/submissions/{pending}")
        if not _ours(sub, inputs):
            raise LegError(f"submission {pending} is in progress in Partner Center and is not this "
                           f"version's: it is left as it is — finish or delete it there, then re-run")
        sid = str(pending)
        if sub.get("status") != "PendingCommit":
            return _settle(pc, sid, pc.call("GET", f"/submissions/{sid}/status"), inputs, record, _track(sub))
        print(f"MICROSOFT STORE: resuming submission {sid}, not yet committed")
        filled = sub
    else:
        if not last:
            raise LegError("the app has no published submission to clone")
        created = pc.call("POST", "/submissions")
        sid = str(created["id"])
        filled = pc.call("PUT", f"/submissions/{sid}", fill(created, inputs))
        print(f"MICROSOFT STORE: created submission {sid} from the published one")
    sas = filled.get("fileUploadUrl")
    if not sas:
        raise LegError(f"submission {sid} carries no upload address")
    pc.upload(sas, _upload_zip(inputs))
    pc.call("POST", f"/submissions/{sid}/commit")
    status = _wait_commit(pc, sid, sleep)
    return _settle(pc, sid, status, inputs, record, _track(filled))


def main(argv: list[str] | None = None, env: dict[str, str] | None = None,
         transport: Transport = urllib_transport, sleep: Callable[[float], None] = time.sleep) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--version", required=True,
                    help="the release, X.Y.Z, or `tree` for the version the tree builds (a dry run)")
    ap.add_argument("--packages", required=True, type=Path,
                    help="directory holding Fauna-Store-<arch>.msix and its .sha256 for both arches")
    ap.add_argument("--certification", required=True, type=Path,
                    help="directory holding certification-<arch>.xml for both arches")
    ap.add_argument("--record", required=True, type=Path, help="the run's store-legs.json")
    ap.add_argument("--dry-run", action="store_true", help="check the inputs and stop: no store call")
    a = ap.parse_args(argv)
    try:
        inputs = check_inputs(a.version, a.packages, a.certification)
        print(f"MICROSOFT STORE: inputs good for {inputs.version} (build {inputs.build}, "
              f"furthest {inputs.furthest})")
        if a.dry_run:
            print("MICROSOFT STORE: dry run — no store call")
            return 0
        tenant, client, secret = credential(os.environ if env is None else env)
        submit(PartnerCenter(transport, tenant, client, secret), inputs, a.record, sleep)
    except LegError as exc:
        print(f"::error::Microsoft Store leg: {exc}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
