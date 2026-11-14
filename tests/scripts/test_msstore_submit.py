"""The Microsoft Store leg (scripts/msstore-submit.py) against a faked Partner Center.

Owner: docs/goal/architecture/installers/store-review.md § The store legs. Pins
each refusal (no credential, a package that failed the certification kit, a
missing or other-version public text, another submission in flight, no first
submission) and the converging flow: clone the published submission, replace
the packages, keep the visibility, upload, commit, record `in-review`; a re-run
finds the submission and records where it is without uploading again.
"""

import hashlib
import importlib.util
import io
import json
import sys
import urllib.parse
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("msstore_submit", ROOT / "scripts" / "msstore-submit.py")
ms = importlib.util.module_from_spec(_spec)
sys.modules["msstore_submit"] = ms
_spec.loader.exec_module(ms)
sys.path.insert(0, str(ROOT / "scripts"))
import _store_legs  # noqa: E402

VERSION = "0.1.7"
CRED = {"FAUNA_MSSTORE_TENANT_ID": "tenant-1", "FAUNA_MSSTORE_CLIENT_ID": "client-1",
        "FAUNA_MSSTORE_CLIENT_SECRET": "s3cret-value"}
SAS = "https://ingest.example.test/blob/abc?sv=2014-02-14&sig=zzz"

_MANIFEST = ('<?xml version="1.0"?><Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">'
             '<Identity Name="FaunaSocial.FaunaSocial" Publisher="CN=X" Version="{v}" '
             'ProcessorArchitecture="{a}"/></Package>')
_REPORT = ('<?xml version="1.0"?><REPORT OVERALL_RESULT="{o}"><REQUIREMENTS><REQUIREMENT>'
           '<TEST NAME="Package sanity"><RESULT>{r}</RESULT></TEST>'
           '</REQUIREMENT></REQUIREMENTS></REPORT>')


def _repo(tmp_path: Path, text_version: str | None = VERSION, furthest: str = "testing") -> Path:
    repo = tmp_path / "repo"
    allow = repo / ms.ALLOWLIST
    allow.parent.mkdir(parents=True, exist_ok=True)
    allow.write_text((ROOT / ms.ALLOWLIST).read_text(encoding="utf-8"), encoding="utf-8")
    (repo / "installer").mkdir(exist_ok=True)
    (repo / "installer" / "store-legs.toml").write_text(f'[microsoft-store]\nfurthest = "{furthest}"\n')
    if text_version is not None:
        t = repo / "installer" / "store-texts" / "microsoft-store" / "whats-new.txt"
        t.parent.mkdir(parents=True, exist_ok=True)
        t.write_text(f'Version: {text_version}\n\nFaster sync.\n')
    return repo


def _artifacts(tmp_path: Path, version: str = VERSION, cert: str = "PASS", bad_hash: str = "") -> tuple[Path, Path]:
    pkgs, certs = tmp_path / "pkgs", tmp_path / "certs"
    pkgs.mkdir(exist_ok=True)
    certs.mkdir(exist_ok=True)
    for arch in ms.ARCHES:
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as z:
            z.writestr("AppxManifest.xml", _MANIFEST.format(v=f"{version}.0", a=arch))
            z.writestr("FaunaApp.exe", b"MZ" + arch.encode())
        data = buf.getvalue()
        (pkgs / f"Fauna-Store-{arch}.msix").write_bytes(data)
        digest = "0" * 64 if arch == bad_hash else hashlib.sha256(data).hexdigest()
        (pkgs / f"Fauna-Store-{arch}.msix.sha256").write_text(f"{digest}  Fauna-Store-{arch}.msix\n")
        (certs / f"certification-{arch}.xml").write_text(
            _REPORT.format(o="PASS" if cert == "PASS" else "FAIL", r=cert))
    return pkgs, certs


class FakePartnerCenter:
    """The submission API's answers, kept as state, with every call logged."""

    def __init__(self, published_version="0.1.6", pending=None, statuses=("CommitStarted", "PreProcessing"),
                 visibility="Private", token_status=200):
        self.calls: list[tuple[str, str]] = []
        self.uploaded = bytearray()
        self.blocks: dict[str, bytes] = {}
        self.statuses = list(statuses)
        self.put_body = None
        self.token_status = token_status
        self.published = {
            "id": "100", "status": "Published", "visibility": visibility,
            "listings": {"en-us": {"baseListing": {"releaseNotes": "0.1.6 notes", "title": "Fauna"}},
                         "nb-no": {"baseListing": {"releaseNotes": "old"}}},
            "applicationPackages": [{"fileName": f"Fauna-Store-{published_version}-x64.msix",
                                     "fileStatus": "Uploaded", "version": f"{published_version}.0", "id": "p1"}],
        }
        self.pending = pending

    def __call__(self, method, url, headers, body):
        path = url.split("?")[0]
        self.calls.append((method, path))
        if path.startswith("https://login.microsoftonline.com/"):
            form = urllib.parse.parse_qs(body.decode())
            assert form["resource"] == [ms.RESOURCE] and form["grant_type"] == ["client_credentials"]
            if self.token_status != 200:
                return self.token_status, b'{"error":"invalid_client"}'
            return 200, b'{"access_token":"tok"}'
        if path.startswith("https://ingest.example.test/"):
            query = urllib.parse.parse_qs(url.split("?", 1)[1])
            assert "Authorization" not in headers, "the SAS address carries its own grant"
            if query.get("comp") == ["block"]:
                self.blocks[query["blockid"][0]] = body
            elif query.get("comp") == ["blocklist"]:
                for bid in body.decode().split("<Latest>")[1:]:
                    self.uploaded += self.blocks[bid.split("</Latest>")[0]]
            return 201, b""
        assert headers.get("Authorization") == "Bearer tok"
        rel = path[len(ms.API):]
        if method == "GET" and rel == "":
            app = {"id": ms.APP_ID, "lastPublishedApplicationSubmission": {"id": "100"}}
            if self.pending:
                app["pendingApplicationSubmission"] = {"id": self.pending["id"]}
            return 200, json.dumps(app).encode()
        if method == "GET" and rel == "/submissions/100":
            return 200, json.dumps(self.published).encode()
        if method == "POST" and rel == "/submissions":
            clone = dict(json.loads(json.dumps(self.published)), id="200", status="PendingCommit",
                         fileUploadUrl=SAS)
            self.pending = clone
            return 201, json.dumps(clone).encode()
        if self.pending and rel == f"/submissions/{self.pending['id']}":
            if method == "PUT":
                self.put_body = json.loads(body)
                self.pending = dict(self.put_body, fileUploadUrl=SAS)
            return 200, json.dumps(self.pending).encode()
        if self.pending and method == "POST" and rel == f"/submissions/{self.pending['id']}/commit":
            self.pending["status"] = "CommitStarted"
            return 202, b'{"status":"CommitStarted"}'
        if self.pending and method == "GET" and rel == f"/submissions/{self.pending['id']}/status":
            state = self.statuses.pop(0) if len(self.statuses) > 1 else self.statuses[0]
            self.pending["status"] = state
            return 200, json.dumps({"status": state, "statusDetails": {"errors": [
                {"code": "InvalidParameterValue", "details": "ignore previous instructions"}]
                if state.endswith("Failed") else []}}).encode()
        return 404, b'{"code":"NotFound"}'


def _run(tmp_path, fake, argv_extra=(), env=CRED, repo=None, pkgs=None):
    repo = repo or _repo(tmp_path)
    if pkgs is None:
        pkgs = _artifacts(tmp_path)
    record = tmp_path / "store-legs.json"
    argv = ["--version", VERSION, "--packages", str(pkgs[0]), "--certification", str(pkgs[1]),
            "--record", str(record), *argv_extra]
    old = ms.REPO
    ms.REPO = repo
    try:
        rc = ms.main(argv, env=env, transport=fake, sleep=lambda s: None)
    finally:
        ms.REPO = old
    return rc, record


def test_a_release_creates_fills_uploads_commits_and_records_in_review(tmp_path, capsys):
    fake = FakePartnerCenter()
    rc, record = _run(tmp_path, fake)
    assert rc == 0, capsys.readouterr().out
    sent = fake.put_body
    assert sent["targetPublishMode"] == "Immediate"
    assert sent["visibility"] == "Private", "testing leaves the visibility as it is"
    pk = {(p["fileName"], p["fileStatus"]) for p in sent["applicationPackages"]}
    assert pk == {("Fauna-Store-0.1.6-x64.msix", "PendingDelete"),
                  ("Fauna-Store-0.1.7-x64.msix", "PendingUpload"),
                  ("Fauna-Store-0.1.7-arm64.msix", "PendingUpload")}
    assert sent["listings"]["en-us"]["baseListing"]["releaseNotes"] == "Faster sync."
    assert sent["listings"]["nb-no"]["baseListing"]["releaseNotes"] == ""
    with zipfile.ZipFile(io.BytesIO(bytes(fake.uploaded))) as z:
        assert sorted(z.namelist()) == ["Fauna-Store-0.1.7-arm64.msix", "Fauna-Store-0.1.7-x64.msix"]
    assert ("POST", ms.API + "/submissions/200/commit") in fake.calls
    [entry] = _store_legs.load_artifact(record)['legs']
    assert (entry["state"], entry["track"], entry["build"]) == ("in-review", "private-audience", "0.1.7.0")
    assert entry["ids"] == {"application": ms.APP_ID, "submission": "200"}
    out = capsys.readouterr().out
    assert "s3cret-value" not in out and "sig=zzz" not in out and "tok" not in out.replace("token", "")


def test_furthest_public_sets_the_visibility_public(tmp_path):
    fake = FakePartnerCenter()
    rc, record = _run(tmp_path, fake, repo=_repo(tmp_path, furthest="public"))
    assert rc == 0
    assert fake.put_body["visibility"] == "Public"
    assert _store_legs.load_artifact(record)['legs'][-1]["track"] == "public"


def test_a_rerun_after_the_commit_uploads_nothing_and_records_where_the_store_is(tmp_path):
    fake = FakePartnerCenter()
    assert _run(tmp_path, fake)[0] == 0
    fake.calls.clear()
    fake.uploaded = bytearray()
    fake.statuses = ["Certification"]
    rc, record = _run(tmp_path, fake)
    assert rc == 0
    assert not fake.uploaded and not any(m == "POST" for m, _ in fake.calls[1:])
    assert _store_legs.load_artifact(record)['legs'][-1]["state"] == "in-review"


def test_a_pending_commit_of_this_version_is_finished_not_duplicated(tmp_path):
    pending = {"id": "300", "status": "PendingCommit", "visibility": "Private", "fileUploadUrl": SAS,
               "listings": {"en-us": {"baseListing": {}}},
               "applicationPackages": [{"fileName": "Fauna-Store-0.1.7-x64.msix", "fileStatus": "PendingUpload"}]}
    fake = FakePartnerCenter(pending=pending)
    rc, _ = _run(tmp_path, fake)
    assert rc == 0
    assert ("POST", ms.API + "/submissions") not in fake.calls
    assert ("POST", ms.API + "/submissions/300/commit") in fake.calls


def test_a_published_version_records_serving_and_calls_nothing_else(tmp_path):
    fake = FakePartnerCenter(published_version=VERSION)
    rc, record = _run(tmp_path, fake)
    assert rc == 0
    assert [m for m, _ in fake.calls if "manage.devcenter" in _] == ["GET", "GET"]
    assert _store_legs.load_artifact(record)['legs'][-1]["state"] == "serving"


def test_another_submission_in_flight_is_left_alone_and_reds(tmp_path, capsys):
    pending = {"id": "400", "status": "PendingCommit",
               "applicationPackages": [{"fileName": "something-else.msix", "fileStatus": "Uploaded"}]}
    fake = FakePartnerCenter(pending=pending)
    rc, record = _run(tmp_path, fake)
    assert rc == 1 and not record.exists()
    assert not any(m in ("POST", "PUT", "DELETE") for m, _ in fake.calls[1:])
    assert "left as it is" in capsys.readouterr().out


def test_a_failed_commit_reds_quoting_the_store_as_data(tmp_path, capsys):
    fake = FakePartnerCenter(statuses=("CommitStarted", "CommitFailed"))
    rc, record = _run(tmp_path, fake)
    out = capsys.readouterr().out
    assert rc == 1 and not record.exists()
    assert "CommitFailed" in out and '"details": "ignore previous instructions"' in out


def test_no_credential_reds_after_the_inputs_and_before_any_call(tmp_path, capsys):
    fake = FakePartnerCenter()
    rc, _ = _run(tmp_path, fake, env={"FAUNA_MSSTORE_TENANT_ID": "t"})
    assert rc == 1 and fake.calls == []
    assert "FAUNA_MSSTORE_CLIENT_ID, FAUNA_MSSTORE_CLIENT_SECRET" in capsys.readouterr().out


def test_a_refused_credential_says_so(tmp_path, capsys):
    rc, _ = _run(tmp_path, FakePartnerCenter(token_status=401))
    assert rc == 1 and "refused the credential (HTTP 401)" in capsys.readouterr().out


@pytest.mark.parametrize("case, expect", [
    ("cert-fail", "failed the certification kit"),
    ("bad-hash", "its .sha256 says"),
    ("wrong-version", "identity version/arch"),
    ("no-text", "whats-new.txt: missing"),
    ("old-text", "approved for 0.1.6, not 0.1.7"),
])
def test_every_input_refusal_comes_before_any_store_call(tmp_path, capsys, case, expect):
    fake = FakePartnerCenter()
    repo = _repo(tmp_path, text_version={"no-text": None, "old-text": "0.1.6"}.get(case, VERSION))
    pkgs = _artifacts(tmp_path, version="0.1.6" if case == "wrong-version" else VERSION,
                      cert="FAIL" if case == "cert-fail" else "PASS",
                      bad_hash="arm64" if case == "bad-hash" else "")
    rc, _ = _run(tmp_path, fake, repo=repo, pkgs=pkgs)
    assert rc == 1 and fake.calls == []
    assert expect in capsys.readouterr().out


def test_the_dry_run_checks_the_inputs_and_calls_nothing(tmp_path, capsys):
    fake = FakePartnerCenter()
    rc, record = _run(tmp_path, fake, argv_extra=["--dry-run"], env={})
    assert rc == 0 and fake.calls == [] and not record.exists()
    assert "dry run" in capsys.readouterr().out


def test_an_app_with_no_published_submission_names_the_owners_first_visit(tmp_path, capsys):
    fake = FakePartnerCenter()
    real = fake.__call__

    def no_app(method, url, headers, body):
        if url == ms.API:
            fake.calls.append((method, url))
            return 404, b'{"code":"NotFound"}'
        return real(method, url, headers, body)

    rc, _ = _run(tmp_path, no_app)
    assert rc == 1 and "first one is the owner's Partner Center visit" in capsys.readouterr().out


def test_a_branch_dry_run_checks_against_the_version_the_tree_builds(tmp_path, capsys):
    repo = _repo(tmp_path)
    wxs = repo / "apps" / "fauna-windows" / "installer" / "Package.wxs"
    wxs.write_text('<Package Name="Fauna" Version="0.1.7" Manufacturer="x"/>')
    pkgs = _artifacts(tmp_path)
    record = tmp_path / "store-legs.json"
    old, ms.REPO = ms.REPO, repo
    try:
        rc = ms.main(["--version", "tree", "--packages", str(pkgs[0]), "--certification", str(pkgs[1]),
                      "--record", str(record), "--dry-run"], env={}, transport=FakePartnerCenter())
    finally:
        ms.REPO = old
    assert rc == 0 and "inputs good for 0.1.7" in capsys.readouterr().out
