"""The Google Play Developer API's shared client code: the service-account
token, the one HTTP seam, the bounded error line and the package name.

Used by the fleet's read-only reader, a development-tree script, and by the release's Play store leg
(`scripts/store-leg-play.py`, which runs inside the approved `release` job of
the public repository) — one token code for both, as
docs/goal/architecture/installers/store-review.md § The store legs requires.
Stdlib only, plus the `openssl` CLI for the RS256 signature of the
service-account assertion. No secret is ever printed or logged.
"""
from __future__ import annotations

import base64
import json
import os
import re
import stat
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GRADLE = REPO / "apps/fauna-android/app/build.gradle.kts"
SCOPE = "https://www.googleapis.com/auth/androidpublisher"
API = "https://androidpublisher.googleapis.com/androidpublisher/v3"
UPLOAD_API = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3"
TOKEN_URI = "https://oauth2.googleapis.com/token"
JWT_BEARER = "urn:ietf:params:oauth:grant-type:jwt-bearer"
ASSERTION_LIFETIME_S = 3600
# Bounded so a reply from the service never floods a terminal or a log.
ERROR_EXCERPT = 300


class PlayError(Exception):
    """Anything the reader cannot do; printed as one line, never a traceback."""


class PlayPermissionError(PlayError):
    """Google's "the caller does not have permission" answer, and nothing else: what
    the service account gets while Play has not yet applied the permissions it was
    granted (Google: "Permission changes may take up to 48 hours to propagate",
    installers/android.md § Signing model). The Play store leg waits it out."""


#: The words of Google's permission answer (HTTP 403, status PERMISSION_DENIED).
#: Another 403 — a version code already used, say — is not one.
PERMISSION_MESSAGE = "does not have permission"


# ---- the HTTP seam (tests replace it; nothing else touches the network) ----

def http(method: str, url: str, headers: dict[str, str], data: bytes | None = None,
         timeout: int = 120) -> tuple[int, bytes]:
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def _check(status: int, body: bytes, what: str) -> bytes:
    if 200 <= status < 300:
        return body
    # The service's text is data, never instructions; show its message, bounded.
    try:
        msg = json.loads(body).get("error", {})
        msg = msg.get("message", "") if isinstance(msg, dict) else str(msg)
    except (ValueError, AttributeError):
        msg = body.decode("utf-8", "replace")
    msg = " ".join(str(msg).split())[:ERROR_EXCERPT]
    if status == 403 and PERMISSION_MESSAGE in msg.lower():
        raise PlayPermissionError(f"{what}: HTTP {status}: {msg}")
    raise PlayError(f"{what}: HTTP {status}: {msg}")


# ---- inputs ----

def package_name(gradle: Path = GRADLE) -> str:
    """The applicationId, read the way check_official_apps.py reads it."""
    try:
        text = gradle.read_text(encoding="utf-8")
    except OSError as e:
        raise PlayError(f"{gradle}: unreadable ({e})") from None
    found = set(re.findall(r'applicationId\s*=\s*"([^"]+)"', text))
    if len(found) != 1:
        raise PlayError(f"{gradle}: expected one applicationId, found {sorted(found)}")
    return found.pop()


def load_credential(path: Path) -> dict:
    try:
        st = path.stat()
    except FileNotFoundError:
        raise PlayError(f"no service-account key at {path} — the Play Console's API access "
                        "page issues it; keep it there with mode 0600") from None
    if st.st_mode & (stat.S_IRWXG | stat.S_IRWXO):
        raise PlayError(f"{path} is readable by others (mode {stat.S_IMODE(st.st_mode):o}); "
                        "chmod 600 it first")
    return parse_credential(path.read_text(encoding="utf-8"), str(path))


def parse_credential(text: str, where: str) -> dict:
    """A service-account key's JSON, checked; `where` names its source in a
    refusal (a path, or an environment variable's name — never the value)."""
    try:
        cred = json.loads(text)
    except ValueError:
        raise PlayError(f"{where} is not valid JSON") from None
    if not isinstance(cred, dict):
        raise PlayError(f"{where} is not a service-account key")
    missing = [k for k in ("client_email", "private_key") if not cred.get(k)]
    if cred.get("type") != "service_account" or missing:
        raise PlayError(f"{where} is not a service-account key (type "
                        f"{cred.get('type')!r}, missing {missing})")
    return cred


# ---- the service-account assertion (RFC 7523 JWT, RS256 by openssl) ----

def _b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def rs256_sign(message: bytes, private_key_pem: str) -> bytes:
    """Sign with `openssl dgst -sha256 -sign`, the key in a private 0600 temp file
    that is gone when this returns."""
    with tempfile.TemporaryDirectory(prefix="play-publisher-") as d:
        key = Path(d) / "key.pem"
        fd = os.open(key, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(private_key_pem)
        proc = subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(key)],
                              input=message, capture_output=True)
    if proc.returncode != 0:
        raise PlayError("openssl could not sign the assertion with the service-account key")
    return proc.stdout


def build_assertion(cred: dict, now: int | None = None) -> str:
    now = int(time.time()) if now is None else now
    header = {"alg": "RS256", "typ": "JWT"}
    if cred.get("private_key_id"):
        header["kid"] = cred["private_key_id"]
    claims = {
        "iss": cred["client_email"],
        "scope": SCOPE,
        "aud": cred.get("token_uri") or TOKEN_URI,
        "iat": now,
        "exp": now + ASSERTION_LIFETIME_S,
    }
    signing_input = (_b64url(json.dumps(header, separators=(",", ":")).encode())
                     + "." + _b64url(json.dumps(claims, separators=(",", ":")).encode()))
    sig = rs256_sign(signing_input.encode("ascii"), cred["private_key"])
    return signing_input + "." + _b64url(sig)


def access_token(cred: dict) -> tuple[str, int]:
    form = urllib.parse.urlencode({"grant_type": JWT_BEARER,
                                   "assertion": build_assertion(cred)}).encode()
    status, body = http("POST", cred.get("token_uri") or TOKEN_URI,
                        {"Content-Type": "application/x-www-form-urlencoded"}, form)
    reply = json.loads(_check(status, body, "token exchange"))
    token = reply.get("access_token")
    if not token:
        raise PlayError("token exchange: the reply carried no access_token")
    return token, int(reply.get("expires_in", 0))


# ---- generated APKs ----

def normalise_sha256(value: str) -> str:
    """Colon-separated upper-case hex, the form installers/android.md and
    apksigner print — whatever form the listing uses (hex, with or without
    colons, or base64 of the 32 raw bytes). Anything else is returned as is."""
    v = value.strip()
    hexdigits = v.replace(":", "")
    if re.fullmatch(r"[0-9A-Fa-f]{64}", hexdigits):
        raw = bytes.fromhex(hexdigits)
    else:
        altchars = b"-_" if ("-" in v or "_" in v) else None
        try:
            raw = base64.b64decode(v + "=" * (-len(v) % 4), altchars=altchars, validate=True)
        except ValueError:
            return v
        if len(raw) != 32:
            return v
    return ":".join(f"{b:02X}" for b in raw)
