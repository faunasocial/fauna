"""tier_1: the Google Play store leg (scripts/store-leg-play.py) with the Play
Developer API faked — every refusal it owes, the converging edit, the result it
hands the release attempt, and the upload key's pin.

Owner: docs/goal/architecture/installers/store-review.md § The store legs.
No network: the one HTTP seam (scripts/_play_api.py `http`) is replaced.
"""
from __future__ import annotations

import datetime as _dt
import importlib.util
import json
import re
import sys
import zipfile
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "scripts"))

import _play_api as api  # noqa: E402
import _store_legs as legs  # noqa: E402

_spec = importlib.util.spec_from_file_location("store_leg_play", REPO / "scripts" / "store-leg-play.py")
leg = importlib.util.module_from_spec(_spec)
sys.modules["store_leg_play"] = leg
_spec.loader.exec_module(leg)

VERSION = "0.1.6"
CODE = 106
NOW = _dt.datetime(2026, 11, 12, 9, 0, tzinfo=_dt.timezone.utc)


def _aab(path: Path, signed: bool) -> Path:
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("base/manifest/AndroidManifest.xml", b"\x0a\x00")
        z.writestr("base/dex/classes.dex", b"dex")
        if signed:
            z.writestr("META-INF/FAUNA-UP.RSA", b"sig")
    return path


def _texts(root: Path, version: str = VERSION, text: str = "Fixes and polish.") -> Path:
    """installer/store-texts in scripts/_store_legs.py's grammar."""
    d = root / "play"
    d.mkdir(parents=True)
    (d / "whats-new.txt").write_text(f"Version: {version}\n\n{text}\n", encoding="utf-8")
    return root


def _cargo(path: Path, version: str = VERSION) -> Path:
    path.write_text(f'[workspace.package]\nversion = "{version}"\n', encoding="utf-8")
    return path


def _settings(path: Path, play: str = "testing") -> Path:
    path.write_text(f'[play]\nfurthest = "{play}"\n[app-store]\nfurthest = "testing"\n'
                    '[microsoft-store]\nfurthest = "testing"\n', encoding="utf-8")
    return path


# ---- the fake store ----

class FakePlay:
    """An in-memory Play: tracks, bundles, edits; records every call."""

    def __init__(self, internal=None, bundles=None, production=None, read_code=CODE):
        self.tracks = {"internal": internal or [], "production": production or []}
        self.bundles = list(bundles or [])
        self.calls: list[tuple[str, str]] = []
        self.pending: dict[str, dict] = {}
        self.read_code = read_code
        self.n = 0

    def __call__(self, method, url, headers, data=None, timeout=120):
        path = url.split("/applications/social.fauna.fauna", 1)[1]
        self.calls.append((method, path))
        assert headers["Authorization"] == "Bearer tok"
        if method == "POST" and path == "/edits":
            self.n += 1
            eid = f"e{self.n}"
            self.pending[eid] = {"tracks": {}, "bundles": []}
            return 200, json.dumps({"id": eid}).encode()
        m = re.fullmatch(r"/edits/(e\d+)(.*)", path)
        eid, rest = m.group(1), m.group(2)
        if method == "DELETE" and rest == "":
            self.pending.pop(eid)
            return 204, b""
        if method == "GET" and rest.startswith("/tracks/"):
            t = rest.split("/")[-1]
            return 200, json.dumps({"track": t, "releases": self.tracks[t]}).encode()
        if method == "GET" and rest == "/bundles":
            return 200, json.dumps({"bundles": self.bundles}).encode()
        if method == "POST" and rest.startswith("/bundles?uploadType=media"):
            assert "/upload/androidpublisher/v3/" in url
            b = {"versionCode": self.read_code, "sha256": __import__("hashlib").sha256(data).hexdigest()}
            self.pending[eid]["bundles"].append(b)
            return 200, json.dumps(b).encode()
        if method == "PUT" and rest.startswith("/tracks/"):
            body = json.loads(data)
            self.pending[eid]["tracks"][body["track"]] = body["releases"]
            return 200, json.dumps(body).encode()
        if method == "POST" and rest == ":commit":
            e = self.pending.pop(eid)
            self.bundles += e["bundles"]
            self.tracks.update(e["tracks"])
            return 200, json.dumps({"id": eid}).encode()
        raise AssertionError(f"unexpected call {method} {path}")

    def acts(self):
        return [c for c in self.calls if c[0] in ("PUT",) or "uploadType" in c[1] or c[1].endswith(":commit")]


@pytest.fixture
def fake(monkeypatch):
    def install(**kw):
        f = FakePlay(**kw)
        monkeypatch.setattr(api, "http", f)
        return f
    return install


def _converge(tmp_path, furthest="testing"):
    aab = _aab(tmp_path / "signed.aab", signed=True)
    text = {"language": "en-US", "text": "Fixes and polish."}
    return leg.converge(leg.Play("tok", "social.fauna.fauna"), version=VERSION, aab=aab,
                        text=text, furthest=furthest, now=lambda: NOW)


# ---- version code ----

def test_version_code_is_the_android_doc_arithmetic():
    assert leg.version_code("0.1.6") == 106
    assert leg.version_code("1.0.0") == 10000
    with pytest.raises(legs.LegError):
        leg.version_code("0.1")


def test_upload_writes_its_legs_into_store_legs_json(monkeypatch, fake, tmp_path):
    fake()
    monkeypatch.setenv(leg.ENV_ACCOUNT, "{}")
    monkeypatch.setattr(api, "parse_credential", lambda text, where: {})
    monkeypatch.setattr(api, "access_token", lambda cred: ("tok", 3599))
    monkeypatch.setattr(api, "package_name", lambda: "social.fauna.fauna")
    out = tmp_path / "store-legs.json"
    leg.upload(VERSION, _aab(tmp_path / "s.aab", signed=True), out,
               texts=_texts(tmp_path / "texts"), settings=_settings(tmp_path / "s.toml"))
    data = legs.load_artifact(out)
    assert [(l["store"], l["track"], l["state"]) for l in data["legs"]] == [
        ("play", "internal-testing", "serving")]


# ---- converging upload ----

def test_a_fresh_version_is_uploaded_released_on_internal_and_read_back(fake, tmp_path):
    play = fake()
    results = _converge(tmp_path)
    assert play.tracks["internal"] == [{
        "name": VERSION, "versionCodes": ["106"], "status": "completed",
        "releaseNotes": [{"language": "en-US", "text": "Fixes and polish."}]}]
    assert results == [{"store": "play", "version": VERSION, "build": "106",
                        "track": "internal-testing", "state": "serving",
                        "read": "2026-11-12T09:00Z", "ids": {"edit": "e1"}, "note": ""}]
    assert not legs.leg_problems(results[0]), "the leg is in the shared grammar"
    assert not play.pending, "every edit is committed or deleted"
    assert play.tracks["production"] == [], "0.1.x stops at internal testing"


def test_a_version_the_track_already_serves_uploads_nothing(fake, tmp_path):
    play = fake(internal=[{"versionCodes": ["106"], "status": "completed"}],
                bundles=[{"versionCode": CODE}])
    results = _converge(tmp_path)
    assert play.acts() == [], "a re-run converges: no upload, no track write, no commit"
    assert results[0]["state"] == "serving" and results[0]["ids"] == {}
    assert not play.pending


def test_a_version_code_play_holds_elsewhere_is_refused_before_any_upload(fake, tmp_path):
    play = fake(bundles=[{"versionCode": CODE}])
    with pytest.raises(legs.LegError, match="already holds version code 106"):
        _converge(tmp_path)
    assert play.acts() == []
    assert not play.pending, "the read edit is deleted"


def test_a_draft_on_internal_is_refused_too(fake, tmp_path):
    play = fake(internal=[{"versionCodes": ["106"], "status": "draft"}])
    with pytest.raises(legs.LegError, match="already holds"):
        _converge(tmp_path)
    assert play.acts() == []


def test_a_bundle_play_reads_as_another_version_is_never_committed(fake, tmp_path):
    play = fake(read_code=105)
    with pytest.raises(legs.LegError, match="version code 105"):
        _converge(tmp_path)
    assert play.tracks["internal"] == [] and play.bundles == []
    assert not play.pending, "the edit is abandoned, never committed"


def test_furthest_public_releases_on_production_which_is_review(fake, tmp_path):
    play = fake()
    results = _converge(tmp_path, furthest="public")
    assert [r["track"] for r in results] == ["internal-testing", "production"]
    assert results[1]["state"] == "in-review"
    assert play.tracks["production"][0]["versionCodes"] == ["106"]
    assert not play.pending


def test_an_http_error_is_one_bounded_line_and_leaves_no_edit_open(monkeypatch, fake, tmp_path):
    play = fake()
    real = play.__call__

    def failing(method, url, headers, data=None, timeout=120):
        if "uploadType" in url:
            play.calls.append((method, "upload"))
            return 403, json.dumps({"error": {"message": "The caller does not have permission " * 40}}).encode()
        return real(method, url, headers, data, timeout)

    monkeypatch.setattr(api, "http", failing)
    with pytest.raises(api.PlayError) as e:
        _converge(tmp_path)
    assert "HTTP 403" in str(e.value) and len(str(e.value)) < api.ERROR_EXCERPT + 60
    assert not play.pending


# ---- refusals before the store is touched ----

def test_upload_refuses_without_the_credential(monkeypatch, fake, tmp_path):
    play = fake()
    monkeypatch.delenv(leg.ENV_ACCOUNT, raising=False)
    monkeypatch.setattr(leg, "CARGO", _cargo(tmp_path / "Cargo.toml"))
    aab = _aab(tmp_path / "s.aab", signed=True)
    with pytest.raises(legs.LegError, match="no credential: FAUNA_PLAY_SERVICE_ACCOUNT_JSON"):
        leg.upload(VERSION, aab, tmp_path / "store-legs.json",
                   texts=_texts(tmp_path / "texts"), settings=_settings(tmp_path / "s.toml"))
    assert play.calls == []


def _check(tmp_path, **kw):
    aab = kw.pop("aab", None) or _aab(tmp_path / "b.aab", signed=kw.pop("signed", False))
    return leg.check(VERSION, aab, texts=kw.pop("texts", None) or _texts(tmp_path / "texts"),
                     cargo=kw.pop("cargo", None) or _cargo(tmp_path / "Cargo.toml"), **kw)


def test_check_passes_an_unsigned_bundle_of_this_version_with_its_text(tmp_path):
    assert _check(tmp_path) == {"language": "en-US", "text": "Fixes and polish."}


def test_check_refuses_a_missing_text(tmp_path):
    (tmp_path / "empty").mkdir()
    with pytest.raises(legs.LegError, match="whats-new.txt: missing"):
        _check(tmp_path, texts=tmp_path / "empty")


def test_check_refuses_a_text_approved_for_another_version(tmp_path):
    with pytest.raises(legs.LegError, match="approved for 0.1.5, not 0.1.6"):
        _check(tmp_path, texts=_texts(tmp_path / "t", version="0.1.5"))


def test_a_version_neutral_text_passes(tmp_path):
    assert _check(tmp_path, texts=_texts(tmp_path / "t", version="any"))["language"] == "en-US"


def test_check_refuses_a_text_over_plays_limit_and_never_shortens(tmp_path):
    with pytest.raises(legs.LegError, match="over Play's 500"):
        _check(tmp_path, texts=_texts(tmp_path / "t", text="x" * 501))


def test_check_refuses_another_trees_version(tmp_path):
    with pytest.raises(legs.LegError, match="this tree's version is 0.1.5"):
        _check(tmp_path, cargo=_cargo(tmp_path / "C.toml", "0.1.5"))


def test_check_refuses_a_signed_bundle_in_the_bundle_job(tmp_path):
    with pytest.raises(legs.LegError, match="is signed"):
        _check(tmp_path, signed=True)


def test_sign_refuses_without_the_keystore(monkeypatch, tmp_path):
    monkeypatch.delenv(leg.ENV_KEYSTORE, raising=False)
    with pytest.raises(legs.LegError, match="no credential: FAUNA_PLAY_UPLOAD_KEYSTORE"):
        leg.sign(_aab(tmp_path / "u.aab", signed=False), tmp_path / "s.aab")


def test_sign_refuses_a_certificate_that_is_not_the_upload_keys(monkeypatch, tmp_path):
    """A keystore whose key is not the upload key never yields an upload."""
    import subprocess
    try:
        jdk = subprocess.run(["keytool", "-help"], capture_output=True).returncode == 0
    except FileNotFoundError:
        jdk = False
    if not jdk:
        # mac ships a `keytool` stub that answers "Unable to locate a Java
        # Runtime"; the leg's JDK steps run only on a box with one
        from helpers.app_surface import skip_environment
        skip_environment("no Java runtime here: keytool cannot make the throwaway keystore")
    ks = tmp_path / "other.jks"
    monkeypatch.setenv(leg.ENV_KEYSTORE_PASSWORD, "pw-for-a-throwaway-key")
    subprocess.run(["keytool", "-genkeypair", "-keystore", str(ks), "-storetype", "PKCS12",
                    "-storepass:env", leg.ENV_KEYSTORE_PASSWORD, "-alias", leg.KEY_ALIAS,
                    "-keyalg", "RSA", "-keysize", "2048", "-validity", "2",
                    "-dname", "CN=not the upload key"], check=True, capture_output=True)
    import base64
    monkeypatch.setenv(leg.ENV_KEYSTORE, base64.b64encode(ks.read_bytes()).decode())
    out = tmp_path / "signed.aab"
    with pytest.raises(legs.LegError, match="not the upload key's"):
        leg.sign(_aab(tmp_path / "u.aab", signed=False), out)
    assert not out.exists(), "a wrongly signed bundle is not left behind"


def test_the_upload_key_pin_is_the_android_docs():
    doc = (REPO / "docs/goal/architecture/installers/android.md").read_text(encoding="utf-8")
    assert f"**Upload key** (registered by the first upload): SHA-256 `{leg.UPLOAD_CERT_SHA256}`" in doc


# ---- a permission refusal is waited out, inside the job (installers/android.md
# § Signing model: Google may take up to 48 hours to apply the service account's
# release permissions; store-review.md § The store legs) ----

_DENIED = json.dumps({"error": {"code": 403, "status": "PERMISSION_DENIED",
                                "message": "The caller does not have permission"}}).encode()
_USED = b'{"error": {"message": "APK specifies a version code that has already been used."}}'


def test_only_googles_permission_answer_is_a_permission_refusal():
    with pytest.raises(api.PlayPermissionError):
        api._check(403, _DENIED, "edits.insert")
    with pytest.raises(api.PlayPermissionError):
        api._check(403, b'{"error": {"message": "The caller does not have permission"}}', "x")
    unauth = b'{"error": {"status": "UNAUTHENTICATED", "message": "Request had invalid credentials."}}'
    for status, body in ((403, _USED), (401, unauth), (500, b"oops"), (404, b"{}")):
        with pytest.raises(api.PlayError) as e:
            api._check(status, body, "x")
        assert not isinstance(e.value, api.PlayPermissionError), (status, body)


class _Clock:
    """A fake clock the fake sleep moves: no real wait in a test."""

    def __init__(self):
        self.t = 0.0
        self.slept: list[float] = []

    def __call__(self):
        return self.t

    def sleep(self, s):
        self.slept.append(s)
        self.t += s

    def now(self):
        return NOW + _dt.timedelta(seconds=self.t)


def _denying(monkeypatch, play, refusals):
    """Play refuses permission on the first `refusals` edits.insert calls."""
    real = play.__call__
    left = [refusals]

    def http(method, url, headers, data=None, timeout=120):
        if method == "POST" and url.endswith("/edits") and left[0] > 0:
            left[0] -= 1
            play.calls.append((method, "denied"))
            return 403, _DENIED
        return real(method, url, headers, data, timeout)

    monkeypatch.setattr(api, "http", http)


def _patient(tmp_path, clock, tries):
    aab = _aab(tmp_path / "signed.aab", signed=True)
    text = {"language": "en-US", "text": "Fixes and polish."}
    return leg.converge_patiently(leg.Play("tok", "social.fauna.fauna"), version=VERSION,
                                  aab=aab, text=text, furthest="testing", tries=tries,
                                  clock=clock, sleep=clock.sleep, now=clock.now)


def test_a_permission_refusal_is_waited_out_and_each_try_is_recorded(monkeypatch, fake, tmp_path, capsys):
    play = fake()
    _denying(monkeypatch, play, 2)
    clock, tries = _Clock(), []
    results = _patient(tmp_path, clock, tries)
    assert results[0]["state"] == "serving" and play.tracks["internal"]
    assert clock.slept == [leg.PERMISSION_WAIT_S] * 2
    assert [t["outcome"] for t in tries] == ["refused-permission", "refused-permission", "done"]
    assert [t["at"] for t in tries] == ["2026-11-12T09:00Z", "2026-11-12T09:05Z", "2026-11-12T09:10Z"]
    assert all(not legs.try_problems(t) for t in tries)
    assert capsys.readouterr().out.count("Play refused permission") == 2, "every try is printed"


def test_the_wait_stops_inside_the_jobs_time_limit_with_the_stores_only_run_named(monkeypatch, fake, tmp_path):
    play = fake()
    _denying(monkeypatch, play, 10_000)
    clock, tries = _Clock(), []
    with pytest.raises(legs.LegError, match="stores_commit") as e:
        _patient(tmp_path, clock, tries)
    assert "48 hours" in str(e.value)
    assert sum(clock.slept) <= leg.PERMISSION_BUDGET_S
    assert leg.PERMISSION_BUDGET_S <= 25 * 60, "the release job's limit is 60 minutes in all"
    assert all(t["outcome"] == "refused-permission" for t in tries)
    assert len(tries) == leg.PERMISSION_BUDGET_S // leg.PERMISSION_WAIT_S + 1
    assert play.acts() == [] and not play.pending


def test_any_other_failure_is_red_at_once(monkeypatch, fake, tmp_path):
    play = fake()
    real = play.__call__

    def http(method, url, headers, data=None, timeout=120):
        if "uploadType" in url:
            return 403, _USED
        return real(method, url, headers, data, timeout)

    monkeypatch.setattr(api, "http", http)
    clock, tries = _Clock(), []
    with pytest.raises(api.PlayError):
        _patient(tmp_path, clock, tries)
    assert clock.slept == [], "no wait on anything but the permission answer"
    assert [t["outcome"] for t in tries] == ["red"]
    assert not play.pending


def test_upload_writes_its_tries_even_when_it_goes_red(monkeypatch, fake, tmp_path):
    play = fake()
    _denying(monkeypatch, play, 10_000)
    clock = _Clock()
    monkeypatch.setattr(leg, "_clock", clock)
    monkeypatch.setattr(leg, "_sleep", clock.sleep)
    monkeypatch.setattr(leg, "_now", clock.now)
    monkeypatch.setenv(leg.ENV_ACCOUNT, "{}")
    monkeypatch.setattr(api, "parse_credential", lambda text, where: {})
    monkeypatch.setattr(api, "access_token", lambda cred: ("tok", 3599))
    monkeypatch.setattr(api, "package_name", lambda: "social.fauna.fauna")
    out = tmp_path / "store-legs.json"
    with pytest.raises(legs.LegError):
        leg.upload(VERSION, _aab(tmp_path / "s.aab", signed=True), out,
                   texts=_texts(tmp_path / "texts"), settings=_settings(tmp_path / "s.toml"))
    data = legs.load_artifact(out)
    assert data["legs"] == []
    assert {t["store"] for t in data["tries"]} == {"play"} and len(data["tries"]) > 1
