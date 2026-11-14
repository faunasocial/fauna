#!/usr/bin/env python3
"""The Google Play store leg: sign the release's `storeNoPayments` bundle with
the upload key and release it on Play's internal testing track, by itself.

Owner: docs/goal/architecture/installers/store-review.md § The store legs
(where the leg runs, its credential, how far it goes, the texts, the record);
the bundle, its signing and its version code: installers/android.md.

It runs in the public repository's release workflow:

  check  --version V --aab A
      In the bundle job, which holds no credential (and again in the approved
      job): the bundle is the unsigned AAB of this tree's version, and Play's
      *What's new* text for V is present and approved for V. A dry run stops
      here — it performs no store act.

  sign   --aab A --out S
      A step of the approved `release` job: sign with the upload key
      (FAUNA_PLAY_UPLOAD_KEYSTORE, base64; FAUNA_PLAY_UPLOAD_KEYSTORE_PASSWORD;
      alias `fauna-upload`) by the JDK's own `jarsigner`, then read the signed
      bundle's certificate back and refuse unless its SHA-256 is the upload
      key's, recorded in installers/android.md.

  upload --version V --aab S --result store-legs.json
      A step of the approved `release` job: with the service account
      (FAUNA_PLAY_SERVICE_ACCOUNT_JSON) read the store first and converge —
      a version code the internal track already serves is success and uploads
      nothing; one Play holds anywhere else is a red (Play never takes a version
      code twice); otherwise one edit uploads the bundle, releases it on
      `internal` with the text, and commits. The track is read back, and the
      state reached — with the store's identifiers, never a secret — is added
      to the run artifact store-legs.json, which the release attempt's `stores`
      step writes into installer/store-review.toml. With `furthest = "public"`
      (installer/store-legs.toml) the same version code is then released on
      `production` at full rollout, which submits it to Google's review.
      Google's "the caller does not have permission" answer — what the
      service account gets until Play applies the release permissions it was
      granted, which Google says may take up to 48 hours — is waited out: the
      leg tries again every PERMISSION_WAIT_S for up to PERMISSION_BUDGET_S,
      printing each try, and goes red after that naming the stores-only run
      that finishes it; any other failure is red at once. Every try is added
      to store-legs.json's `tries`, red or not (store-review.md § The store
      legs, *A refusal of permission is waited out*).

No third-party client: the Play Developer API over HTTPS with the token code of
scripts/_play_api.py. Stdlib, `openssl`, and the JDK's `jarsigner`/`keytool`.
"""
from __future__ import annotations

import argparse
import base64
import datetime as _dt
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.parse
import zipfile
from pathlib import Path

import _play_api as api
import _store_legs as legs

REPO = Path(__file__).resolve().parent.parent
CARGO = REPO / "Cargo.toml"

STORE = "play"
TEXT_KIND, = legs.LEG_TEXTS[STORE]["testing"]  # one text on every track
#: Play's release notes: one language, at most 500 characters. The text file
#: carries no language (scripts/_store_legs.py's grammar), so the leg names it.
TEXT_LANGUAGE = "en-US"
TEXT_MAX = 500
KEY_ALIAS = "fauna-upload"

ENV_ACCOUNT = "FAUNA_PLAY_SERVICE_ACCOUNT_JSON"
ENV_KEYSTORE = "FAUNA_PLAY_UPLOAD_KEYSTORE"
ENV_KEYSTORE_PASSWORD = "FAUNA_PLAY_UPLOAD_KEYSTORE_PASSWORD"

#: The upload key's certificate SHA-256, as installers/android.md § Implementation
#: status today records it (the Signing bullet and the Play signing keys bullet);
#: a pin test holds the two equal.
UPLOAD_CERT_SHA256 = ("59:E2:E7:F0:56:EE:DA:BA:79:49:3A:9C:E6:26:5E:D1:"
                      "4C:93:08:52:D8:DA:66:33:B5:C4:61:E8:84:25:85:12")

#: Play's track names → the store-review record's.
TRACKS = {"internal": "internal-testing", "production": "production"}

UPLOAD_TIMEOUT_S = 900

#: The wait on Play's permission answer (store-review.md § The store legs, *A refusal
#: of permission is waited out*): a try every 5 minutes for 20 minutes. The `release`
#: job's whole limit is 60 minutes (release.yml), shared with the download publishing
#: before the Play steps and the Microsoft Store leg after them, so the wait stays
#: well inside it; a longer wait — Google allows itself up to 48 hours — is finished
#: by the stores-only run, at one approval per run.
PERMISSION_WAIT_S = 300
PERMISSION_BUDGET_S = 20 * 60
ERROR_EXCERPT = api.ERROR_EXCERPT


def check_version(version: str) -> str:
    if not re.fullmatch(r"\d+\.\d+\.\d+", version or ""):
        raise legs.LegError(f"version {version!r} is not MAJOR.MINOR.PATCH")
    return version


def version_code(version: str) -> int:
    """installers/android.md § Versioning: major * 10000 + minor * 100 + patch."""
    major, minor, patch = (int(x) for x in check_version(version).split("."))
    return major * 10000 + minor * 100 + patch


def tree_version(cargo: Path = CARGO) -> str:
    data = tomllib.loads(cargo.read_text(encoding="utf-8"))
    return data["workspace"]["package"]["version"]


def _signed(aab: Path) -> bool:
    with zipfile.ZipFile(aab) as z:
        return any(re.fullmatch(r"META-INF/[^/]+\.(RSA|DSA|EC)", n) for n in z.namelist())


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ---- check: the inputs, no credential ----

def check(version: str, aab: Path, *, texts: Path = legs.TEXTS, cargo: Path | None = CARGO,
          want_signed: bool = False) -> dict:
    """`cargo` None skips the tree's version (the approved job, which checks the
    version code Play reads from the bundle instead)."""
    check_version(version)
    found = version if cargo is None else tree_version(cargo)
    if found != version:
        raise legs.LegError(f"this tree's version is {found}, not {version}: the bundle "
                            "is built from the version's own commit")
    if not aab.is_file():
        raise legs.LegError(f"no bundle at {aab}")
    try:
        with zipfile.ZipFile(aab) as z:
            if "base/manifest/AndroidManifest.xml" not in z.namelist():
                raise legs.LegError(f"{aab} is not an Android App Bundle (no base manifest)")
    except zipfile.BadZipFile:
        raise legs.LegError(f"{aab} is not a zip archive") from None
    if _signed(aab) != want_signed:
        raise legs.LegError(f"{aab} is {'signed' if not want_signed else 'unsigned'}: the "
                            "bundle job builds it unsigned and only the approved job signs it")
    text = legs.read_text(STORE, TEXT_KIND, version, texts)
    if len(text) > TEXT_MAX:
        raise legs.LegError(f"Play's {TEXT_KIND} text is {len(text)} characters, over Play's "
                            f"{TEXT_MAX}; the leg never shortens a text")
    return {"language": TEXT_LANGUAGE, "text": text}


# ---- sign: the upload key, in the approved job ----

def _need_env(name: str) -> str:
    value = os.environ.get(name, "")
    if not value.strip():
        raise legs.LegError(f"no credential: {name} is not set — it rests only in the "
                            "public repository's `release` Environment")
    return value


def cert_sha256(signed: Path) -> str:
    """The signer certificate's SHA-256, read back by `keytool -printcert -jarfile`."""
    proc = subprocess.run(["keytool", "-printcert", "-jarfile", str(signed)],
                          capture_output=True, text=True)
    found = set(re.findall(r"SHA256:\s*([0-9A-F:]{95})", proc.stdout))
    if proc.returncode != 0 or len(found) != 1:
        raise legs.LegError(f"could not read one signer certificate back from {signed}")
    return found.pop()


def sign(aab: Path, out: Path) -> str:
    keystore_b64 = _need_env(ENV_KEYSTORE)
    _need_env(ENV_KEYSTORE_PASSWORD)
    if _signed(aab):
        raise legs.LegError(f"{aab} is already signed: the bundle job builds it unsigned")
    try:
        keystore = base64.b64decode("".join(keystore_b64.split()), validate=True)
    except ValueError:
        raise legs.LegError(f"{ENV_KEYSTORE} is not base64") from None
    with tempfile.TemporaryDirectory(prefix="store-leg-play-") as d:
        ks = Path(d) / "upload.jks"
        fd = os.open(ks, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as f:
            f.write(keystore)
        # The password reaches jarsigner through the environment, never argv.
        proc = subprocess.run(
            ["jarsigner", "-keystore", str(ks), "-storetype", "PKCS12",
             "-storepass:env", ENV_KEYSTORE_PASSWORD,
             "-sigalg", "SHA256withRSA", "-digestalg", "SHA-256",
             "-signedjar", str(out), str(aab), KEY_ALIAS],
            capture_output=True, text=True)
    if proc.returncode != 0:
        raise legs.LegError("jarsigner refused to sign the bundle with the upload key "
                            "(wrong keystore, password or alias)")
    got = cert_sha256(out)
    if got != UPLOAD_CERT_SHA256:
        out.unlink(missing_ok=True)
        raise legs.LegError(f"the signed bundle's certificate is {got}, not the upload "
                            f"key's {UPLOAD_CERT_SHA256}: refused before any upload")
    return got


# ---- upload: the Play Developer API, converging ----

class Play:
    """The handful of edit calls the leg makes; every reply's text is data."""

    def __init__(self, token: str, package: str):
        self.auth = {"Authorization": f"Bearer {token}"}
        self.base = f"{api.API}/applications/{urllib.parse.quote(package)}"
        self.upload_base = f"{api.UPLOAD_API}/applications/{urllib.parse.quote(package)}"

    def _json(self, method: str, url: str, what: str, body: dict | None = None) -> dict:
        headers = dict(self.auth)
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        status, reply = api.http(method, url, headers, data)
        raw = api._check(status, reply, what)
        return json.loads(raw) if raw.strip() else {}

    def insert_edit(self) -> str:
        edit = self._json("POST", f"{self.base}/edits", "edits.insert", {})
        if not edit.get("id"):
            raise legs.LegError("edits.insert: the reply carried no edit id")
        return edit["id"]

    def delete_edit(self, edit: str) -> None:
        self._json("DELETE", f"{self.base}/edits/{edit}", "edits.delete")

    def track(self, edit: str, track: str) -> dict:
        return self._json("GET", f"{self.base}/edits/{edit}/tracks/{track}", f"tracks.get {track}")

    def bundles(self, edit: str) -> list[dict]:
        return self._json("GET", f"{self.base}/edits/{edit}/bundles", "bundles.list").get("bundles") or []

    def upload_bundle(self, edit: str, aab: Path) -> dict:
        headers = dict(self.auth)
        headers["Content-Type"] = "application/octet-stream"
        status, reply = api.http("POST", f"{self.upload_base}/edits/{edit}/bundles?uploadType=media",
                                 headers, aab.read_bytes(), timeout=UPLOAD_TIMEOUT_S)
        return json.loads(api._check(status, reply, "bundles.upload"))

    def update_track(self, edit: str, track: str, release: dict) -> dict:
        return self._json("PUT", f"{self.base}/edits/{edit}/tracks/{track}", f"tracks.update {track}",
                          {"track": track, "releases": [release]})

    def commit(self, edit: str) -> dict:
        return self._json("POST", f"{self.base}/edits/{edit}:commit", "edits.commit")


def serving(track: dict, code: int) -> bool:
    """Whether a completed (fully rolled out) release of the track carries `code`."""
    return any(r.get("status") == "completed" and str(code) in (r.get("versionCodes") or [])
               for r in track.get("releases") or [])


def anywhere(track: dict, code: int) -> bool:
    return any(str(code) in (r.get("versionCodes") or []) for r in track.get("releases") or [])


def _release(version: str, code: int, text: dict) -> dict:
    return {"name": version, "versionCodes": [str(code)], "status": "completed",
            "releaseNotes": [{"language": text["language"], "text": text["text"]}]}


def _leg(version: str, code: int, track: str, state: str, read: _dt.datetime,
         ids: dict[str, str]) -> dict:
    """One leg in store-legs.json's grammar (scripts/_store_legs.py)."""
    stamp = read.astimezone(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%MZ")
    leg = {"store": STORE, "version": version, "build": str(code), "track": TRACKS[track],
           "state": state, "read": stamp, "ids": dict(ids), "note": ""}
    bad = legs.leg_problems(leg)
    if bad:
        raise legs.LegError("the leg is refused: " + "; ".join(bad))
    return leg


def converge(play: Play, *, version: str, aab: Path, text: dict, furthest: str,
             now=lambda: _dt.datetime.now(_dt.timezone.utc)) -> list[dict]:
    """Bring Play to the state `furthest` asks for; return the results reached."""
    code = version_code(version)
    results: list[dict] = []
    edit = play.insert_edit()
    committed = False
    ids: dict[str, str] = {}
    try:
        internal = play.track(edit, "internal")
        if serving(internal, code):
            print(f"store-leg-play: internal testing already serves {code}; nothing uploaded")
        else:
            held = [b for b in play.bundles(edit) if b.get("versionCode") == code]
            if held or anywhere(internal, code):
                raise legs.LegError(
                    f"Play already holds version code {code} outside a serving internal "
                    "release: an upload spends its code, so this version ships at the next "
                    "patch (product-version.md § Reships)")
            uploaded = play.upload_bundle(edit, aab)
            if uploaded.get("versionCode") != code:
                raise legs.LegError(f"Play read version code {uploaded.get('versionCode')} "
                                    f"from the bundle, not {code}: the edit is abandoned")
            if uploaded.get("sha256") and uploaded["sha256"].lower() != sha256_file(aab):
                raise legs.LegError("Play's SHA-256 of the uploaded bundle is not the "
                                    "signed bundle's: the edit is abandoned")
            play.update_track(edit, "internal", _release(version, code, text))
            play.commit(edit)
            committed = True
            ids["edit"] = edit
            print(f"store-leg-play: uploaded {code} and released it on internal testing")
    finally:
        if not committed:
            play.delete_edit(edit)

    # Read the track back from the store: the state recorded is the store's reading.
    edit = play.insert_edit()
    try:
        internal = play.track(edit, "internal")
        read_at = now()
        if not serving(internal, code):
            raise legs.LegError(f"internal testing does not serve {code} after the commit")
        results.append(_leg(version, code, "internal", "serving", read_at, ids))
        if furthest != "public":
            return results
        production = play.track(edit, "production")
    finally:
        play.delete_edit(edit)

    # furthest = "public": the same version code on production, which Google reviews.
    edit = play.insert_edit()
    committed = False
    try:
        if not anywhere(production, code):
            play.update_track(edit, "production", _release(version, code, text))
            play.commit(edit)
            committed = True
            print(f"store-leg-play: released {code} on production; Google reviews it")
        results.append(_leg(version, code, "production",
                            "in-review" if not serving(production, code) else "serving",
                            now(), {"edit": edit} if committed else {}))
    finally:
        if not committed:
            play.delete_edit(edit)
    return results


def _minute(at: _dt.datetime) -> str:
    return at.astimezone(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%MZ")


# The clock, the sleep and the wall clock of the wait — module names, so a test moves them.
_clock = time.monotonic
_sleep = time.sleep


def _now() -> _dt.datetime:
    return _dt.datetime.now(_dt.timezone.utc)


def converge_patiently(play: Play, *, version: str, aab: Path, text: dict, furthest: str,
                       tries: list[dict], clock=None, sleep=None, now=None) -> list[dict]:
    """`converge`, with Play's permission answer waited out: each try is appended to
    `tries` in store-legs.json's grammar, and printed. `converge` reads the store first
    and leaves no edit open, so a try after a refusal is safe."""
    clock, sleep, now = clock or _clock, sleep or _sleep, now or _now
    start = clock()
    while True:
        at = _minute(now())
        try:
            results = converge(play, version=version, aab=aab, text=text, furthest=furthest,
                               now=now)
        except api.PlayPermissionError as e:
            n = len(tries) + 1
            waited = clock() - start
            tries.append({"store": STORE, "at": at, "outcome": "refused-permission",
                          "note": str(e)[:legs.NOTE_MAX]})
            if waited + PERMISSION_WAIT_S > PERMISSION_BUDGET_S:
                raise legs.LegError(
                    f"Play still refuses the service account permission after {n} tries over "
                    f"{int(waited // 60)} minutes ({e}). Google may take up to 48 hours to "
                    "apply a permission granted in the Play Console: once it has, finish the "
                    "store legs with a stores-only run — release.yml dispatched at this tag "
                    "with stores_commit set to the commit the version records "
                    "(installers/store-review.md § The store legs)") from None
            print(f"store-leg-play: try {n}: Play refused permission ({e}); "
                  f"trying again in {PERMISSION_WAIT_S // 60} minutes")
            sleep(PERMISSION_WAIT_S)
            continue
        except (legs.LegError, api.PlayError) as e:
            tries.append({"store": STORE, "at": at, "outcome": "red",
                          "note": str(e)[:legs.NOTE_MAX]})
            raise
        tries.append({"store": STORE, "at": at, "outcome": "done", "note": ""})
        if len(tries) > 1:
            print(f"store-leg-play: try {len(tries)}: done")
        return results


def upload(version: str, aab: Path, result_path: Path, *, texts: Path = legs.TEXTS,
           settings: Path = legs.SETTINGS) -> list[dict]:
    # Every input is checked before the store is touched.
    text = check(version, aab, texts=texts, cargo=None, want_signed=True)
    how_far = legs.furthest(STORE, settings)
    cred = api.parse_credential(_need_env(ENV_ACCOUNT), ENV_ACCOUNT)
    token, _ = api.access_token(cred)
    tries: list[dict] = []
    try:
        results = converge_patiently(Play(token, api.package_name()), version=version,
                                     aab=aab, text=text, furthest=how_far, tries=tries)
    finally:
        legs.add_tries(result_path, tries)
    for r in results:
        legs.add_leg(result_path, r)
    return results


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="store-leg-play", description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("check", help="the inputs, no credential (a dry run stops here)")
    c.add_argument("--version", required=True)
    c.add_argument("--aab", type=Path, required=True)
    c.add_argument("--tree", type=Path, default=REPO,
                   help="the checkout the bundle was built from (a stores-only run builds "
                        "the recorded commit beside the workflow's own tree)")
    s = sub.add_parser("sign", help="sign with the upload key and verify its certificate")
    s.add_argument("--aab", type=Path, required=True)
    s.add_argument("--out", type=Path, required=True)
    u = sub.add_parser("upload", help="release on internal testing, converging")
    u.add_argument("--version", required=True)
    u.add_argument("--aab", type=Path, required=True)
    u.add_argument("--result", type=Path, required=True)
    args = ap.parse_args(argv)
    try:
        if args.cmd == "check":
            check(args.version, args.aab, cargo=args.tree / "Cargo.toml")
            print(f"store-leg-play: inputs for {args.version} pass "
                  f"(unsigned bundle, version code {version_code(args.version)}, text approved)")
        elif args.cmd == "sign":
            got = sign(args.aab, args.out)
            print(f"store-leg-play: signed with the upload key {got}")
        else:
            for r in upload(args.version, args.aab, args.result):
                print(f"store-leg-play: {r['track']} {r['state']} {r['version']} ({r['build']})")
        return 0
    except (legs.LegError, api.PlayError) as e:
        print(f"store-leg-play: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
