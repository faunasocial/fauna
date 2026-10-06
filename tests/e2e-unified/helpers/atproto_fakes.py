"""In-process fakes for the two external services the ATProto PDS bridge
resolves against: the PLC directory and DNS.

Both stand in via **test-only env seams** — ``FAUNA_ATPROTO_PLC_DIRECTORY_URL``
and ``FAUNA_ATPROTO_FAKE_DNS_URL`` — which are artifact/test IPC, never
operator configuration (§ Product invariants), and which since 2026-09-13 are
**compiled only into the bridge's e2e flavor** (``-tags fauna_e2e_fixtures``;
the shipped flavor has no reader for either name — convention 15,
``e2e-automation-surface-gating.md`` → the Go bridges' leg), so a test that
sets them requests the ``atproto_bridge_e2e_binary`` fixture. The bridge's
third harness seam, ``FAUNA_ATPROTO_PROXY_FIXTURES`` (the fake AppView,
``test_atproto_pds_proxy.py``), is gated the same way. Both are deliberately *URLs*
rather than canned tables, because both answers change during a test run: the
DID does not exist until the mint completes, and a real deployment publishes
the ``_atproto`` TXT only after that. Fixed-at-exec fakes could not express
"the record appeared later", which is exactly what the first-emit
resolvability gate's retry needs (``atproto-pds-full.md`` § Ecosystem reality,
first-impression trap).

Neither fake bypasses anything: they supply *answers*, and the bridge's real
``VerifyIdentityResolvable`` still decides.
"""

from __future__ import annotations

import base64
import hashlib
import json
import threading
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def _now_rfc3339() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _cbor_head(major: int, n: int) -> bytes:
    if n < 24:
        return bytes([(major << 5) | n])
    if n < 0x100:
        return bytes([(major << 5) | 24, n])
    if n < 0x10000:
        return bytes([(major << 5) | 25]) + n.to_bytes(2, "big")
    if n < 0x100000000:
        return bytes([(major << 5) | 26]) + n.to_bytes(4, "big")
    return bytes([(major << 5) | 27]) + n.to_bytes(8, "big")


def dag_cbor(value) -> bytes:
    """Canonical dag-cbor of a JSON-shaped value.

    Map keys are sorted **length-first, then bytewise** — the ordering
    ``fauna_protocol::encode_canonical`` produces and the bridge's Go encoder
    agrees on, which is what makes the CID this fake mints identical to the one
    the client recomputes.

    PLC operations are all strings, arrays, maps and ``null``; ints are handled
    for completeness and floats are refused outright (dag-cbor forbids them).
    """
    if value is None:
        return b"\xf6"
    if isinstance(value, bool):
        return b"\xf5" if value else b"\xf4"
    if isinstance(value, int):
        if value < 0:
            return _cbor_head(1, -value - 1)
        return _cbor_head(0, value)
    if isinstance(value, str):
        raw = value.encode()
        return _cbor_head(3, len(raw)) + raw
    if isinstance(value, list):
        return _cbor_head(4, len(value)) + b"".join(dag_cbor(v) for v in value)
    if isinstance(value, dict):
        items = sorted(value.items(), key=lambda kv: (len(kv[0].encode()), kv[0].encode()))
        out = _cbor_head(5, len(items))
        for k, v in items:
            raw = k.encode()
            out += _cbor_head(3, len(raw)) + raw + dag_cbor(v)
        return out
    raise TypeError(f"dag-cbor cannot encode {type(value).__name__}: {value!r}")


def _base32_lower_nopad(raw: bytes) -> str:
    return base64.b32encode(raw).decode().lower().rstrip("=")


def op_cid(op: dict) -> str:
    """The real CIDv1 of a **signed** PLC operation.

    ``<v1><dag-cbor><sha2-256 multihash>``, multibase-base32 — the same
    derivation the bridge mints with and, since 2026-08-02, the same one the
    client verifies against (``fauna-client-atproto::plc_chain``). The fake used to assign ``bafyfakeop…`` placeholders here; those
    could never bind an entry's content to its identifier, so a tampered
    ``operation`` was indistinguishable from a real one.
    """
    digest = hashlib.sha256(dag_cbor(op)).digest()
    return "b" + _base32_lower_nopad(bytes([0x01, 0x71, 0x12, 0x20]) + digest)


def plc_did(genesis_op: dict) -> str:
    """The ``did:plc:`` a signed genesis operation derives — the root of trust."""
    digest = hashlib.sha256(dag_cbor(genesis_op)).digest()
    return "did:plc:" + _base32_lower_nopad(digest)[:24]


_B58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"

# The group orders the low-S rule is judged against, keyed by the
# `cryptography` curve name.
_CURVE_ORDER = {
    "secp256r1": 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551,
    "secp256k1": 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141,
}


def _b58_decode(s: str) -> bytes:
    """base58btc — did:key's multibase. A dozen lines beat a dependency here
    for the same reason the Rust twin writes out `dag_cbor_cid_v1`: the job is
    binding bytes to an identifier, so the smallest trusted base wins."""
    n = 0
    for c in s:
        n = n * 58 + _B58_ALPHABET.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    pad = len(s) - len(s.lstrip("1"))
    return b"\x00" * pad + raw


def did_key_public_key(did_key: str):
    """``did:key:z…`` → a ``cryptography`` EC public key.

    The two did:plc rotation-key curves: P-256 (multicodec ``0x1200``, prefix
    bytes ``80 24``, strings ``zDn…``) and K-256 (``0xe7``, prefix ``e7 01``,
    strings ``zQ3s…``) — the same table
    ``fauna_protocol::atproto::decode_did_key`` matches on the Rust side.
    """
    from cryptography.hazmat.primitives.asymmetric import ec

    multikey = did_key.removeprefix("did:key:")
    if not multikey.startswith("z"):
        raise ValueError(f"not multibase-base58btc: {did_key!r}")
    raw = _b58_decode(multikey[1:])
    if raw[:2] == b"\x80\x24":
        curve = ec.SECP256R1()
    elif raw[:2] == b"\xe7\x01":
        curve = ec.SECP256K1()
    else:
        raise ValueError(f"unsupported multicodec in {did_key!r}")
    return ec.EllipticCurvePublicKey.from_encoded_point(curve, raw[2:])


def verify_op_sig(op: dict, authorizing_keys: list) -> tuple[bool, str]:
    """Judge ``op``'s signature the way ``plc.directory`` accepts one.

    ECDSA-SHA256 over the op's unsigned canonical dag-cbor, fixed-size
    ``r‖s`` base64url-no-pad, judged against the rotation keys that AUTHORIZE
    the op — the genesis's own list, or the list of the op ``prev`` names.
    Deliberately an *independent* implementation (Python ``cryptography``,
    this module's own dag-cbor): a signature-encoding or canonicalization
    drift in a production signer fails here, in a test, rather than at the
    real directory at the moment the remedy is used.

    Low-S is REQUIRED: the fake models the acceptor, and the real directory
    refuses the malleable ``(r, n−s)`` twin at submission — unlike the Rust
    client's read-side ``verify_op_sig``, which deliberately accepts high-S
    in already-published history. Returns ``(ok, reason)``; the reason is
    written to be quoted in a 400 body, so refusals diagnose themselves.
    """
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives.asymmetric.utils import encode_dss_signature

    sig_b64 = op.get("sig")
    if not isinstance(sig_b64, str) or not sig_b64:
        return False, "the op carries no sig field"
    try:
        sig = base64.urlsafe_b64decode(sig_b64 + "=" * (-len(sig_b64) % 4))
    except Exception:
        return False, f"sig is not base64url: {sig_b64[:32]!r}…"
    if len(sig) != 64:
        return False, f"sig is {len(sig)} bytes, not the fixed-size 64-byte r‖s pair"
    r = int.from_bytes(sig[:32], "big")
    s = int.from_bytes(sig[32:], "big")
    if not authorizing_keys:
        return False, "no rotation keys authorize this op (empty authorizing set)"
    msg = dag_cbor({k: v for k, v in op.items() if k != "sig"})
    der = encode_dss_signature(r, s)
    unparseable = []
    for did_key in authorizing_keys:
        try:
            pub = did_key_public_key(did_key)
        except ValueError:
            unparseable.append(did_key)
            continue
        try:
            pub.verify(der, msg, ec.ECDSA(hashes.SHA256()))
        except InvalidSignature:
            continue
        if s > _CURVE_ORDER[pub.curve.name] // 2:
            return False, (
                f"the signature verifies under {did_key} only as the high-S twin — "
                "plc.directory refuses malleable signatures at submission "
                "(atproto's low-S rule)"
            )
        return True, ""
    detail = f" ({len(unparseable)} listed key(s) unparseable: {unparseable})" if unparseable else ""
    return False, (
        "the signature verifies under none of the authorizing rotation keys "
        f"{authorizing_keys}{detail} over the op's unsigned dag-cbor"
    )


class FakePlcDirectory:
    """Records every ``POST /{did}`` a signer submits, validating like the
    real directory: 200 on acceptance, a self-diagnosing 400 otherwise.

    ``GET /{did}`` answers 200 once that DID was submitted (the bridge's
    post-mint resolvability check) and 404 before.

    ``GET /{did}/log/audit`` serves the operation log the rename hook reads to
    learn the DID's CURRENT state and the ``prev`` CID to chain to. Each
    accepted op gets its **real** dag-cbor CID (:func:`op_cid`) and the fake
    **enforces the chain rule** — an op whose ``prev`` names no entry in the log
    is rejected 400, exactly as the real directory would.

    ⚠ The CIDs used to be ``bafyfakeop…`` placeholders. That was invisible until
    the client started binding an entry's content to its identifier: a placeholder CID cannot detect a tampered
    ``operation``, so the fake could not have modelled the attack the contest
    now defends against — nor can a client verify against it. The
    :func:`dag_cbor` encoder here is pinned to the same vector as
    ``fauna-client-atproto::plc_chain``'s Rust twin, because the contest only
    works if all the encoders agree.

    **Every submitted op must carry a signature an authorizing rotation key
    made**: the genesis verifies
    against its own ``rotationKeys``, every later op against the list of the
    op its ``prev`` names, through the independent :func:`verify_op_sig` —
    so a signature-encoding or canonicalization drift in a production signer
    fails HERE, in a test, not at the real directory at the moment the remedy
    is used. Acceptance pins: ``test_plc_acceptance_parity.py`` and its Rust
    twin vector.

    **Forks are accepted, and nullify what they displace** (the 72 h recovery
    fork, ``atproto-pds-bridge.md`` § State & data shape). An op whose ``prev``
    names an *earlier* entry rather than the head is a fork: the real directory
    accepts it when the signer out-ranks the displaced op's, and marks the
    displaced suffix ``nullified``. This fake judges a fork by the chain rule
    and the signature gate — it does **not** re-derive PLC's *seniority*
    ruling (which listed key out-ranks which) or the 72 h window: that
    ordering logic is exactly the Rust code under test, and a second
    unreviewed implementation of it here could mask the first's bugs. What it
    models honestly is the *consequence* a contest is judged by: the client's
    own next read must see the hostile op gone from the standing chain, which
    is what takes the alarm down (decision 8). Only F5 live interop proves
    the real ``plc.directory``.
    """

    def __init__(self):
        self.submissions = []  # (did, decoded-json-op)
        self.rejected = []  # (did, submitted-prev, expected-head) — chain-rule misses
        self.sig_rejected = []  # (did, reason) — signature-gate refusals
        self.forks = []  # (did, fork-point-cid, [nullified cids]) — accepted forks
        # Every `GET /{did}/log/audit` a CLIENT makes, as
        # (did, senior_key_was_tampered_at_read_time). Mirrors FakeDNS.lookups.
        #
        # This is the read feeder #1's custody check makes, so it is the one
        # observation that splits a silent custody alarm in two: no entry after
        # a tamper means the client never asked (env seam / trigger / gating),
        # while entries after a tamper mean it asked and the break is
        # downstream (verdict, registry, or rendering). Without it the two are
        # indistinguishable from the outside, which is what made the windows
        # leg a guess-and-rerun at ~80 min a go.
        self.audit_reads: list[tuple[str, bool]] = []
        # did -> [ {cid, op, nullified, created_at} ], oldest first.
        self._log: dict[str, list[dict]] = {}
        # S4-C tamper hook: when set, served audit logs substitute this key at
        # rotationKeys[0] (see tamper_senior_key).
        self._tampered_senior_key: str | None = None
        # feeder #3 tamper hook: when set, served audit logs substitute this
        # list at alsoKnownAs (see tamper_also_known_as).
        self._tampered_also_known_as: list[str] | None = None
        # Outage hook: when set, every audit-log read answers 503 (see
        # set_unreadable).
        self._unreadable = False
        self._lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def _json(self, status: int, payload) -> None:
                body = json.dumps(payload).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                # The real PLC directory is browser-fetchable (genesis_verify.rs's
                # design assumes a direct client-side HTTPS read, "no nest
                # dependency" — web's wasm custody check runs this exact fetch
                # from the SPA origin), so it serves CORS headers. Without this
                # the fake's response is a same-origin-only 200 that a real
                # server would carry, and Chromium refuses to hand the body to
                # web's fetch() at all — a browser-only failure mode no native
                # driver can see.
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()
                self.wfile.write(body)

            def do_OPTIONS(self):
                # The CORS preflight `_json`'s own comment describes for GET
                # reads is not the whole story: a POST carrying a
                # non-"simple" content-type (`application/json`, submit_fork's
                # own header) is NOT a simple request under the Fetch spec, so
                # the browser sends an OPTIONS preflight FIRST and blocks the
                # real POST entirely unless this answers it — a native driver
                # never sees this at all (no browser, no preflight), which is
                # why the recovery-fork contest's SUBMIT half (unlike every
                # read-only feeder before it) went unexercised through a real
                # browser until this ceremony landed on web. Missing entirely: measured as a client
                # that gets the contest CARD (a GET-only path) but never
                # completes a contest (its ONE POST), silently — Chromium
                # fails a blocked preflight as a network error with no HTTP
                # response to read at all, so `submit_fork`'s `Err` carries no
                # useful detail either.
                self.send_response(204)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
                self.send_header(
                    "Access-Control-Allow-Headers",
                    self.headers.get("Access-Control-Request-Headers", "content-type"),
                )
                self.send_header("Content-Length", "0")
                self.end_headers()

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                body = self.rfile.read(length)
                did = self.path.lstrip("/")
                op = json.loads(body)
                reject = None  # (error, message)
                with outer._lock:
                    entries = outer._log.setdefault(did, [])
                    standing = [e for e in entries if not e["nullified"]]
                    head_cid = standing[-1]["cid"] if standing else None
                    prev = op.get("prev")
                    fork_cut = None
                    if prev == head_cid:
                        pass  # ordinary extension of the chain
                    elif any(e["cid"] == prev for e in standing):
                        # A FORK: `prev` names a standing entry that is not the
                        # head — everything after it is displaced, once the
                        # signature gate below clears the op.
                        fork_cut = next(
                            i for i, e in enumerate(entries) if e["cid"] == prev
                        )
                    else:
                        outer.rejected.append((did, prev, head_cid))
                        reject = ("InvalidPrev", "prev does not match the log head")
                    if reject is None:
                        # The signature gate — the acceptance-parity boundary.
                        # PLC's rule: the genesis is authorized by its own
                        # rotationKeys, every later op (fork included) by the
                        # rotationKeys of the op its `prev` names.
                        if prev is None:
                            authorizing = op.get("rotationKeys") or []
                        else:
                            prev_entry = next(e for e in standing if e["cid"] == prev)
                            authorizing = prev_entry["op"].get("rotationKeys") or []
                        ok, reason = verify_op_sig(op, authorizing)
                        if not ok:
                            outer.sig_rejected.append((did, reason))
                            reject = ("InvalidSignature", reason)
                    if reject is None:
                        if fork_cut is not None:
                            nullified = []
                            for e in entries[fork_cut + 1 :]:
                                if not e["nullified"]:
                                    e["nullified"] = True
                                    nullified.append(e["cid"])
                            outer.forks.append((did, prev, nullified))
                        cid = op_cid(op)
                        entries.append(
                            {
                                "cid": cid,
                                "op": op,
                                "nullified": False,
                                "created_at": _now_rfc3339(),
                            }
                        )
                        outer.submissions.append((did, op))
                if reject:
                    self._json(400, {"error": reject[0], "message": reject[1]})
                    return
                self._json(200, {})

            def do_GET(self):
                path = self.path.lstrip("/")
                if path.endswith("/log/audit"):
                    did = path[: -len("/log/audit")]
                    with outer._lock:
                        unreadable = outer._unreadable
                        if unreadable:
                            outer.audit_reads.append((did, outer._tampered_senior_key is not None))
                    if unreadable:
                        self._json(503, {"error": "directory unavailable (test outage)"})
                        return
                    with outer._lock:
                        entries = [dict(e) for e in outer._log.get(did, [])]
                        tampered = outer._tampered_senior_key
                        tampered_aka = outer._tampered_also_known_as
                        outer.audit_reads.append((did, tampered is not None))
                    rows = []
                    for entry in entries:
                        op = entry["op"]
                        if tampered is not None and op.get("rotationKeys"):
                            op = dict(op)
                            op["rotationKeys"] = [tampered] + list(op["rotationKeys"][1:])
                        if tampered_aka is not None and op.get("type") == "plc_operation":
                            op = dict(op)
                            op["alsoKnownAs"] = list(tampered_aka)
                        rows.append(
                            {
                                "did": did,
                                "cid": entry["cid"],
                                "nullified": entry["nullified"],
                                "createdAt": entry["created_at"],
                                "operation": op,
                            }
                        )
                    self._json(200, rows)
                    return
                with outer._lock:
                    known = path in outer._log and bool(outer._log[path])
                if known:
                    self._json(200, {})
                    return
                self.send_response(404)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()
                self.wfile.write(b"not found")

            def log_message(self, *args):
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self._server.server_address[1]}"

    def snapshot(self):
        with self._lock:
            return list(self.submissions)

    def tamper_senior_key(self, did_key: str | None) -> None:
        """S4-C TAMPER HOOK: serve audit logs whose every op carries
        ``did_key`` at ``rotationKeys[0]`` — a directory record whose senior
        key is NOT the user's, which the client's genesis-seniority check must
        alarm on. ``None`` restores honest serving. The stored log itself is
        untouched (submissions still validate against the real chain).

        ⚠ Rewrites EVERY op, **the genesis included**, so the client reads the
        violation at standing index 0 — a ``GenesisViolation``, which is
        honestly ``not-contestable`` (there is no earlier state to return to).
        That is the right shape for the alarm test and the wrong one for the
        contest: use :meth:`inject_forged_rotation` for a violation above an
        honest genesis."""
        with self._lock:
            self._tampered_senior_key = did_key

    def set_unreadable(self, unreadable: bool) -> None:
        """OUTAGE HOOK: while set, every ``GET /{did}/log/audit`` answers 503,
        so a client's audit read fails the way an unreachable directory does.
        The read is still recorded in ``audit_reads``, so a test can tell "the
        client asked and got nothing" from "the client never asked". An
        unreadable directory is not a resolution: no standing alarm may clear
        on it (``atproto-identity-custody.md`` § The audit floor and the
        departed-DID alarm). ``False`` restores honest serving."""
        with self._lock:
            self._unreadable = unreadable

    def tamper_also_known_as(self, handles: list[str] | None) -> None:
        """Feeder #3 TAMPER HOOK: serve audit logs whose STANDING HEAD op (and
        every other ``plc_operation`` entry — the fake keeps this simple since
        only the head is ever read, ``handle_binding.rs``'s own doc comment)
        carries ``handles`` at ``alsoKnownAs`` instead of whatever was
        genuinely submitted — e.g. a handle at a domain the user does not
        control, or ``[]`` for "claims no handle at all". ``None`` restores
        honest serving. The stored log itself is untouched (submissions still
        validate against the real chain); a ``plc_tombstone`` entry is never
        touched (it carries no ``alsoKnownAs`` field to begin with)."""
        with self._lock:
            self._tampered_also_known_as = handles
    def inject_forged_rotation(self, did_key: str, did: str | None = None) -> str:
        """Append a standing operation that *claims* to seize the identity.

        ⚠ **This produces a FORGED log, not a real seizure** — a distinction
        that did not exist before and now decides the outcome.
        The op carries the previous op's ``sig`` over different bytes, so no key
        listed in the chain ever signed it, and since 2026-08-02 the client
        refuses to build a fork from a log it cannot authenticate.

        Modelling a *real* seizure requires signing with a key the genesis
        lists, i.e. the user's senior key (client-only) or the bridge's junior
        key (bridge-only) — this fake holds neither, which is why a bridge-side
        seam is the next track. Until then this hook exercises the refusal, and
        no test here proves a successful contest.

        The op is the current head with ``rotationKeys[0]`` replaced by
        ``did_key``, chained honestly onto that head. So the log the client
        reads is: an **honest genesis** it can verify, then a hostile op it
        cannot — a violation at standing index ≥ 1, which is exactly the
        ``contestable`` case (the fork point is the entry before it).

        Returns the new op's CID (the value a user consent would be scoped
        to)."""
        with self._lock:
            if did is None:
                dids = [d for d, entries in self._log.items() if entries]
                if len(dids) != 1:
                    raise AssertionError(
                        f"pass did= explicitly: the fake holds {len(dids)} logs"
                    )
                did = dids[0]
            entries = self._log[did]
            standing = [e for e in entries if not e["nullified"]]
            head = standing[-1]
            op = dict(head["op"])
            op["rotationKeys"] = [did_key] + list(op.get("rotationKeys", [])[1:])
            op["prev"] = head["cid"]
            cid = op_cid(op)
            entries.append(
                {
                    "cid": cid,
                    "op": op,
                    "nullified": False,
                    "created_at": _now_rfc3339(),
                }
            )
            return cid

    def standing_rotation_keys(self, did: str) -> list[str]:
        """``rotationKeys`` of the newest STANDING op — what the world currently
        believes about who controls the identity. A successful contest is
        visible here: the hostile op is nullified, so this answers the user's
        own key again."""
        with self._lock:
            standing = [e for e in self._log.get(did, []) if not e["nullified"]]
            return list(standing[-1]["op"].get("rotationKeys", [])) if standing else []

    def close(self):
        self._server.shutdown()
        self._server.server_close()


class FakeDNS:
    """``GET /txt/{name}`` → 200 + JSON array of TXT strings, or 404.

    Mutable at runtime via :meth:`publish`, so a test can make a handle become
    resolvable part-way through and observe the first-emit gate defer and then
    open. A name with no records answers 404 — the honest NXDOMAIN shape a test
    asserting the gate BLOCKS depends on.
    """

    def __init__(self):
        self._records: dict[str, list[str]] = {}
        self._lock = threading.Lock()
        self.lookups: list[str] = []
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                name = self.path[len("/txt/"):]
                with outer._lock:
                    outer.lookups.append(name)
                    records = outer._records.get(name)
                if not records:
                    self.send_response(404)
                    self.end_headers()
                    self.wfile.write(b"[]")
                    return
                body = json.dumps(records).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self._server.server_address[1]}"

    def publish(self, name: str, records: list[str]) -> None:
        with self._lock:
            self._records[name] = records

    def lookup_count(self) -> int:
        with self._lock:
            return len(self.lookups)

    def close(self):
        self._server.shutdown()
        self._server.server_close()


class FakeAppView:
    """A plain-HTTP stand-in AppView for the F3 service-proxy tier_3.

    Reached through the bridge's ``FAUNA_ATPROTO_PROXY_FIXTURES`` seam (same
    env-seam family as the two fakes above): the seam skips DID resolution and
    the SSRF guard for exactly the refs the harness maps here — a loopback
    endpoint is what the guard exists to refuse — while every unmapped ref
    still runs the full guarded production path.

    Records every request (path, query, headers, body) so the test can assert
    what actually crossed: the service JWT's claims, the forwarded query, and
    the absence of the caller's PDS access token.
    """

    def __init__(self, timeline: dict | None = None):
        self.requests = []  # (path, query, headers-dict, body-bytes)
        self._timeline = timeline or {"feed": [], "cursor": ""}
        self._lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def _record(self):
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length) if length else b""
                path, _, query = self.path.partition("?")
                with outer._lock:
                    outer.requests.append((path, query, dict(self.headers), body))
                return path

            def _json(self, status: int, payload) -> None:
                body = json.dumps(payload).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                path = self._record()
                if path == "/xrpc/app.bsky.feed.getTimeline":
                    self._json(200, outer._timeline)
                else:
                    self._json(404, {"error": "MethodNotImplemented",
                                     "message": "fake appview: " + path})

            def do_POST(self):
                self._record()
                self._json(200, {})

            def log_message(self, *args):
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    def snapshot(self):
        with self._lock:
            return list(self.requests)

    def close(self):
        self._server.shutdown()
        self._server.server_close()


#: The header a running nest's test-routed Bluesky OAuth client carries each
#: request's original authority in (`fauna_bridge_atproto::oauth::
#: TEST_ORIGINAL_HOST_HEADER`) — how one loopback server answers for every
#: atproto host the consume-side flow resolves.
ORIGINAL_HOST_HEADER = "x-fauna-test-original-host"


class FakeAtprotoFarEnd:
    """The consume-side atproto far end a *running* nest's Bluesky OAuth client
    talks to — handle resolution, the PLC directory, the PDS's protected-
    resource metadata, the authorization server (metadata, PAR, token), and
    the PDS's XRPC (``getProfile``, ``getTimeline``, ``getFeed``,
    ``getRecord``, ``createRecord``).

    Reached through the nest's ``FAUNA_TEST_ATPROTO_FAR_END`` seam (compiled
    only under ``test-hooks``, loopback-only — ``bins/fauna-nest/src/state.rs``
    ``build_bluesky_oauth_client``): the nest's client sends every request here
    with its original host in :data:`ORIGINAL_HOST_HEADER`. The out-of-process
    twin of the in-process canned far end the tier_3
    ``conformance_bluesky_feed_ingest.rs`` drives. Unlike :class:`FakeAppView`
    (the Go bridge's hosted-proxy seam) this answers the nest's own
    consume-side client, the only one that polls a linked account's feeds.

    It supplies answers and records what crossed; it verifies nothing — the
    nest's real OAuth client, callback and poller decide.
    """

    def __init__(self, *, handle: str = "alice.test",
                 did: str = "did:plc:faketestbridgeuser234567"):
        self.handle = handle
        self.did = did
        self.pds = f"https://pds.{handle}"
        self.issuer = f"https://auth.{handle}"
        self.par_states: list[str] = []
        self.created_records: list[dict] = []
        self.requests: list[tuple[str, str, str]] = []  # (method, host, path)
        self._timeline: list[dict] = []
        self._records: dict[str, dict] = {}  # at-uri -> {"cid", "value"}
        self._lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def _route(self, method: str):
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length) if length else b""
                host = (self.headers.get(ORIGINAL_HOST_HEADER) or "").lower()
                path, _, query = self.path.partition("?")
                with outer._lock:
                    outer.requests.append((method, host, path))
                status, payload, headers = outer._answer(method, host, path, query, body)
                if isinstance(payload, bytes):
                    data, ctype = payload, "text/plain"
                else:
                    data, ctype = json.dumps(payload).encode(), "application/json"
                self.send_response(status)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Length", str(len(data)))
                for k, v in (headers or {}).items():
                    self.send_header(k, v)
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                self._route("GET")

            def do_POST(self):
                self._route("POST")

            def log_message(self, *args):
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    # -- the answers --------------------------------------------------------

    def _answer(self, method, host, path, query, body):
        from urllib.parse import parse_qs

        pds_host = self.pds.removeprefix("https://")
        auth_host = self.issuer.removeprefix("https://")
        nonce = {"DPoP-Nonce": "fake-far-end-nonce"}

        if method == "GET" and host == self.handle and path == "/.well-known/atproto-did":
            return 200, self.did.encode(), None
        if method == "GET" and host == "plc.directory" and path == f"/{self.did}":
            return 200, {
                "id": self.did,
                "alsoKnownAs": [f"at://{self.handle}"],
                "service": [{
                    "id": "#atproto_pds",
                    "type": "AtprotoPersonalDataServer",
                    "serviceEndpoint": self.pds,
                }],
            }, None
        if host == pds_host and path == "/.well-known/oauth-protected-resource":
            return 200, {"resource": self.pds, "authorization_servers": [self.issuer],
                         "scopes_supported": []}, None
        if host == auth_host and path == "/.well-known/oauth-authorization-server":
            return 200, {
                "issuer": self.issuer,
                "authorization_endpoint": f"{self.issuer}/oauth/authorize",
                "token_endpoint": f"{self.issuer}/oauth/token",
                "pushed_authorization_request_endpoint": f"{self.issuer}/oauth/par",
                "require_pushed_authorization_requests": True,
                "scopes_supported": ["atproto", "transition:chat.bsky"],
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["private_key_jwt", "none"],
                "token_endpoint_auth_signing_alg_values_supported": ["ES256"],
                "dpop_signing_alg_values_supported": ["ES256"],
            }, None
        if method == "POST" and host == auth_host and path == "/oauth/par":
            form = parse_qs(body.decode())
            with self._lock:
                self.par_states.extend(form.get("state", []))
            return 201, {"request_uri": "urn:ietf:params:oauth:request_uri:fake",
                         "expires_in": 299}, nonce
        if method == "POST" and host == auth_host and path == "/oauth/token":
            return 200, {
                "access_token": "fake-access-token",
                "token_type": "DPoP",
                "expires_in": 3600,
                "refresh_token": "fake-refresh-token",
                "scope": "atproto transition:chat.bsky",
                "sub": self.did,
            }, nonce
        if host == pds_host and path.startswith("/xrpc/"):
            return self._xrpc(method, path.removeprefix("/xrpc/"), parse_qs(query), body)
        return 404, {"error": "NotFound",
                     "message": f"fake far end: {method} {host}{path}"}, None

    def _xrpc(self, method, nsid, params, body):
        if nsid == "app.bsky.actor.getProfile":
            return 200, {"did": self.did, "handle": self.handle}, None
        if nsid == "app.bsky.feed.getTimeline":
            with self._lock:
                feed = [{"post": p} for p in reversed(self._timeline)]
            return 200, {"feed": feed}, None
        if nsid == "app.bsky.feed.getFeed":
            return 200, {"feed": []}, None
        if nsid == "com.atproto.repo.getRecord":
            uri = (f"at://{params['repo'][0]}/{params['collection'][0]}/"
                   f"{params['rkey'][0]}")
            with self._lock:
                rec = self._records.get(uri)
            if rec is None:
                return 400, {"error": "RecordNotFound", "message": uri}, None
            return 200, {"uri": uri, "cid": rec["cid"], "value": rec["value"]}, None
        if method == "POST" and nsid == "com.atproto.repo.createRecord":
            payload = json.loads(body or b"{}")
            with self._lock:
                self.created_records.append(payload)
                n = len(self.created_records)
            uri = f"at://{self.did}/{payload.get('collection')}/fake{n}"
            return 200, {"uri": uri, "cid": op_cid({"created": n})}, None
        return 400, {"error": "MethodNotImplemented",
                     "message": f"fake far end: {nsid}"}, None

    # -- the test's levers --------------------------------------------------

    def publish_post(self, *, author_did: str, author_handle: str, rkey: str,
                     text: str, image_urls: tuple[str, ...] = ()) -> str:
        """Put a post by someone the linked account follows on its timeline
        (newest last) and in the repo ``getRecord`` reads; returns its AT-URI.

        ``image_urls`` become the post view's ``app.bsky.embed.images#view``
        embed (each url its ``fullsize`` and ``thumb``) — the CDN urls the
        nest's ingest rewrites behind its ``bluesky/media`` proxy."""
        uri = f"at://{author_did}/app.bsky.feed.post/{rkey}"
        record = {"$type": "app.bsky.feed.post", "text": text,
                  "createdAt": _now_rfc3339()}
        cid = op_cid({"uri": uri, "text": text})
        view = {
            "uri": uri, "cid": cid,
            "author": {"did": author_did, "handle": author_handle},
            "record": record, "indexedAt": _now_rfc3339(),
        }
        if image_urls:
            view["embed"] = {
                "$type": "app.bsky.embed.images#view",
                "images": [{"thumb": u, "fullsize": u, "alt": ""} for u in image_urls],
            }
        with self._lock:
            self._records[uri] = {"cid": cid, "value": record}
            self._timeline.append(view)
        return uri

    def created_snapshot(self) -> list[dict]:
        with self._lock:
            return list(self.created_records)

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    def close(self):
        self._server.shutdown()
        self._server.server_close()
