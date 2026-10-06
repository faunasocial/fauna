"""tier_4 (live-remote, OPT-IN, NON-DESTRUCTIVE): the ActivityPub serving
surface on an already-deployed production nest (example.com), driven through
the linux app UI for every mutation and plain public HTTPS GETs (stdlib
``urllib.request``, the suite's convention) for every assertion.

No nest APIs anywhere: every mutation is a linux app UI action (sign in,
link the bridge, create a post, delete the post) and every observation is an
unauthenticated HTTPS read of a route any fediverse crawler or peer could
make (nodeinfo, WebFinger, the actor document, a note, the outbox).

Blast-radius argument (testing.md § shared-box rule, non-destructive
carve-out, ratified 2026-07-22 — required on every non-destructive
``live_box`` test):
  - **What it mutates:** enables the ActivityPub bridge for the admin
    account (one ``ap_accounts`` row), creates ONE feed post carrying a
    unique test marker, deletes that post, unlinks the bridge. The sign-in
    that precedes all of it — including the handle derivation, which types a
    probe handle into the wizard — mutates NOTHING: the wizard's handle check
    is a key-based silent challenge (a read), ``AlreadyOnNest`` registers
    nothing, and the wizard's handle field is not the handle-*change* surface
    (that is Settings → ``change-handle``).
  - **Why invisible to the human:** no factory reset, no credential or
    storage-mode mutation, no logout, no deletion or modification of any
    pre-existing data. A Create-push fans out only to *accepted followers*
    of the enabling actor, and a freshly-enabled actor has none — so nothing
    ever leaves the box. At worst the human sees one transient test post
    appear, then disappear, in their own feed.
  - **What teardown removes:** the test post (via the client UI's own
    delete), then ``bridges.unlink("activitypub")`` — the nest drops the
    ``ap_accounts`` + ``ap_follows`` rows for this actor and frees the
    UNIQUE username (``ActivityPubProvider::unlink`` ->
    ``db_helpers::delete_account``), so re-runs from a clean slate are
    idempotent.
  - **Why it must NEVER factory-reset:** a reset wipes and re-mints the
    nest's ``ap_instance_actor`` RSA key, which silently invalidates the
    cached copy held by EVERY real peer that has ever federated with this
    box — off-box state no teardown can ever reach (the factory-reset
    rotation hazard, docs/goal/behavior/activitypub.md § Architecture ->
    The instance actor). This test therefore treats any pre-existing AP
    state as a hard stop, never something to "fix" by unlinking — see the
    precondition below, which never repairs, only refuses.

Preconditions (env):
  FAUNA_LIVE_NEST_URL       e.g. https://dev.example.com — or the run's own box on a
                            `--nest live:URL` run (``live_box_door.live_box_url``)
Optional:
  FAUNA_LIVE_SECRET_HEX     admin ed25519 32-byte seed hex — an OVERRIDE; by
                            default the seed resolves per box
                            (``multiseat_config.resolve_secret``: the box's
                            staging-box file, then ``~/.fauna-id``)
  FAUNA_LIVE_HANDLE         OVERRIDE only — the admin's handle is DERIVED from
                            the secret (``helpers/live_handle.py``); set this
                            only to target a handle the derivation cannot reach
                            (a handle-less account, or a claim-fresh box).
                            FAUNA_LIVE_MAIL_ADDRESS is read as the same override.
  FAUNA_LIVE_CLAIM_CODE     required only if the nest is UNCLAIMED at start

**The handle is derived, never demanded** (2026-08-16): this test signs in with
the secret and drives the real client UI, so the signed-in admin's own handle is
already in hand — requiring it a second time as out-of-band env input made a
"run this against the live box" ask un-self-servable, since the handle lives in
no artifact in the repo or on any dev machine. The wizard's own handle check
adjudicates it (``AlreadyOnNest`` returns the registered handle and the machine
signs in as *that*, not as what was typed); the mechanism and its two refusal
cases live in ``helpers/live_handle.py``.

Hard preconditions, never repaired (``_assert_no_ap_accounts`` +
``_assert_handle_unfingerable``): the box
must show ZERO AP-enabled accounts (nodeinfo ``usage.users.total == 0``) and
the handle must WebFinger-404 before this test mutates anything. Unlinking
someone's real AP account to "normalize" the box would destroy real
follower rows — that is user data, and this test refuses rather than
repairs.

**Ordering** (a consequence of deriving rather than demanding): the
handle-independent half of the precondition — ``usage.users.total == 0``, which
is the *strong* half, since zero AP-enabled accounts means no handle whatsoever
can resolve — runs first, before anything at all. Sign-in follows, purely to
learn the handle. Only then does the per-handle WebFinger-404 check run. Nothing
is mutated in between: sign-in is a read of the account (silent challenge +
account fetch), it is not the handle-change surface, and it leaves no artifact
— which is exactly what the carve-out below requires of it.

Test taxonomy: tier_4 (live-remote — the deployed production image under
real supervision; ruling 2026-07-22).
"""

from __future__ import annotations

import base64
import json
import os
import time
import urllib.error
import urllib.request
import uuid
from urllib.parse import urlencode

import pytest

from helpers import live_box_door
from helpers.app_surface import skip_unbuilt
from helpers.live_admin import dump, reach_admin_shell, wait_connected
from helpers.live_handle import derive_handle

from i18n.strings import S

# The run's box on a `--nest live` run, else FAUNA_LIVE_NEST_URL
# (`live_box_door.live_box_url`).
URL = live_box_door.live_box_url()
# The admin seed is resolved per box like every live-mode admin
# (`live_box_door.admin_seed` → `resolve_secret`: `FAUNA_LIVE_SECRET_HEX` > the box's staging-box file >
# `~/.fauna-id`), so a staging box this fleet provisioned runs with nothing
# exported. `FAUNA_LIVE_HANDLE` is deliberately not a precondition: the handle is
# derived from the secret, so demanding it would re-impose the out-of-band
# knowledge this test was fixed to stop asking for.
SECRET, SECRET_SOURCE = live_box_door.admin_seed(URL)

pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_nest,
    pytest.mark.live_box,
    pytest.mark.cd_suite,
    pytest.mark.skipif(
        not (URL and SECRET),
        reason="live-nest UI-only ActivityPub serving-surface test: name the box "
        "(`--nest live:URL`, or FAUNA_LIVE_NEST_URL) and provide its admin seed "
        f"({live_box_door.SEED_SOURCES}) to run (the admin handle is derived from the secret; hits a "
        "live external nest; opt-in, non-destructive — see module docstring)",
    ),
]

CLAIM_CODE = os.environ.get("FAUNA_LIVE_CLAIM_CODE", "")

AP_ACCEPT = "application/activity+json"
# DER bytes of the rsaEncryption AlgorithmIdentifier OID (1.2.840.113549.1.1.1)
# — present verbatim in any RSA SubjectPublicKeyInfo regardless of key size.
_RSA_ENCRYPTION_OID = bytes.fromhex("2a864886f70d010101")


# ── HTTP helpers (stdlib only — public reads, no nest API) ─────────────────


def _request(url: str, accept: str | None = None, timeout: float = 20) -> tuple[int, str, dict]:
    """Plain GET. Returns ``(status, raw-body, headers)``. Never raises on a
    4xx/5xx — ``HTTPError`` is caught and its body read, so a caller
    observes a 404/406 exactly the same way it observes a 200 (no
    try/except at every call site). A connection failure (DNS/refused/TLS)
    is a hard ``pytest.fail`` — that is an infrastructure problem, not a
    thing under test.

    Header keys are lower-cased: HTTP header names are case-insensitive and
    hyper serves them lowercase (``content-type``), so a plain-dict lookup
    of ``Content-Type`` would silently read ``''``."""
    req = urllib.request.Request(url, headers={"Accept": accept} if accept else {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            headers = {k.lower(): v for k, v in resp.headers.items()}
            return resp.status, resp.read().decode("utf-8", "replace"), headers
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", "replace") if e.fp else ""
        return e.code, body, {k.lower(): v for k, v in (e.headers or {}).items()}
    except urllib.error.URLError as e:
        pytest.fail(f"GET {url} (Accept: {accept!r}) failed to connect: {e}")


def _get_json(url: str, accept: str | None = None, timeout: float = 20):
    """``(status, parsed-json-or-None, raw-body)``. Never raises on a 4xx —
    callers assert on the status directly (e.g. the WebFinger-404 checks
    below)."""
    status, raw, _headers = _request(url, accept=accept, timeout=timeout)
    try:
        parsed = json.loads(raw) if raw else None
    except json.JSONDecodeError:
        parsed = None
    return status, parsed, raw


def _poll_until(fn, desc: str, timeout: float = 60.0, interval: float = 2.0) -> None:
    """Poll the zero-arg predicate ``fn`` — which returns ``(ok, observed)``
    — until ``ok`` is true, else ``pytest.fail`` naming ``desc`` and the
    LAST ``observed`` value. Live-remote assertions must poll: the box is
    remote and every mutation (bridge enable, post create/delete, bridge
    unlink) lands asynchronously relative to the WS-RPC call that triggered
    it, so a single immediate GET would be a race, not a check. Every
    timeout carries the last observation so it self-diagnoses without a
    screenshot."""
    deadline = time.monotonic() + timeout
    observed = "<predicate never evaluated>"
    while time.monotonic() < deadline:
        ok, observed = fn()
        if ok:
            return
        time.sleep(interval)
    pytest.fail(f"{desc} — not observed within {timeout:.0f}s (last: {observed})")


# ── ActivityPub-specific HTTP helpers ───────────────────────────────────────


def _discover_nodeinfo_href() -> str:
    """Follow ``/.well-known/nodeinfo`` to the version-specific document URL
    — discovered, never hardcoded, so a future schema bump doesn't silently
    stale this test the way the six-week ``/api/v1/node-info`` dangle once
    did in production (actor_routes.rs's own ``nodeinfo`` doc comment)."""
    status, doc, raw = _get_json(f"{URL}/.well-known/nodeinfo")
    assert status == 200 and doc, (
        f"/.well-known/nodeinfo did not return a discovery document: "
        f"status={status} body={raw[:300]!r} — the box answered the admin "
        f"preflight (it is claimed and knows the seed), so a landing page here "
        f"means the ActivityPub routes are not being served: a product signal"
    )
    links = doc.get("links") or []
    assert links, f"/.well-known/nodeinfo returned no links: {raw[:300]!r}"
    href = links[0].get("href")
    assert href, f"/.well-known/nodeinfo links[0] has no href: {raw[:300]!r}"
    return href


def _nodeinfo_users_total() -> tuple[int | None, str]:
    """``(usage.users.total, diagnostic-string)``. Re-discovers the href
    each call — cheap, and avoids trusting a cached href across the test."""
    href = _discover_nodeinfo_href()
    status, doc, raw = _get_json(href)
    total = (doc or {}).get("usage", {}).get("users", {}).get("total")
    return total, f"status={status} usage.users.total={total} body={raw[:250]!r}"


def _users_total_reaches(n: int, timeout: float = 60.0) -> None:
    def predicate():
        total, desc = _nodeinfo_users_total()
        return total == n, desc

    _poll_until(predicate, f"nodeinfo usage.users.total reaches {n}", timeout=timeout)


def _webfinger_url(handle: str) -> str:
    return f"{URL}/.well-known/webfinger?{urlencode({'resource': f'acct:{handle}'})}"


def _webfinger(handle: str) -> tuple[int, dict | None, str]:
    return _get_json(_webfinger_url(handle))


def _webfinger_resolves(handle: str, timeout: float = 60.0) -> dict:
    """Poll until WebFinger resolves (200), then return the parsed doc."""

    def predicate():
        status, doc, raw = _webfinger(handle)
        return status == 200 and doc is not None, f"status={status} body={raw[:250]!r}"

    _poll_until(predicate, f"webfinger resolves for acct:{handle}", timeout=timeout)
    status, doc, raw = _webfinger(handle)
    assert status == 200 and doc, (
        f"webfinger flipped back to non-200 right after the poll observed "
        f"200: status={status} body={raw[:300]!r}"
    )
    return doc


def _webfinger_404s(handle: str, timeout: float = 60.0) -> None:
    def predicate():
        status, _doc, raw = _webfinger(handle)
        return status == 404, f"status={status} body={raw[:250]!r}"

    _poll_until(predicate, f"webfinger 404s for acct:{handle} (username freed)", timeout=timeout)


def _extract_self_actor_url(webfinger_doc: dict) -> str:
    """The ``rel=self``, ``type=application/activity+json`` link — the
    actor document's own address (webfinger.rs's ``build_webfinger_response``
    always emits exactly one such link)."""
    for link in webfinger_doc.get("links") or []:
        link_type = link.get("type") or ""
        if link.get("rel") == "self" and link_type.startswith("application/activity+json"):
            href = link.get("href")
            if href:
                return href
    pytest.fail(f"webfinger response has no rel=self activity+json link: {webfinger_doc!r}")


def _actor_doc(actor_url: str) -> tuple[int, dict | None, str, str]:
    """``(status, parsed-json-or-None, raw-body, content-type)`` — the
    Content-Type is asserted on separately from ``_get_json``'s 3-tuple
    contract, so this fetches directly via ``_request``."""
    status, raw, headers = _request(actor_url, accept=AP_ACCEPT)
    try:
        doc = json.loads(raw) if raw else None
    except json.JSONDecodeError:
        doc = None
    return status, doc, raw, headers.get("content-type", "")


def _assert_rsa_public_key_pem(pem: str) -> None:
    """Structural, stdlib-only proof that ``pem`` is a well-formed RSA
    SubjectPublicKeyInfo: strip the PEM armor, base64-decode the DER, and
    assert the bytes contain the rsaEncryption AlgorithmIdentifier OID
    (1.2.840.113549.1.1.1 -> DER bytes ``2a 86 48 86 f7 0d 01 01 01``). That
    OID sequence appears verbatim in any RSA SPKI DER regardless of key
    size or exact ASN.1 layout, so a raw substring search is a correct (if
    blunt) structural check without a full ASN.1 parser or any third-party
    crypto library."""
    stripped = pem.strip()
    assert stripped.startswith("-----BEGIN PUBLIC KEY-----"), (
        f"publicKeyPem is not PEM-armored: {pem[:80]!r}"
    )
    body = (
        stripped.replace("-----BEGIN PUBLIC KEY-----", "")
        .replace("-----END PUBLIC KEY-----", "")
        .strip()
    )
    try:
        der = base64.b64decode("".join(body.split()))
    except Exception as e:  # noqa: BLE001 — report the bad PEM, not a bare traceback
        pytest.fail(f"publicKeyPem body does not base64-decode: {e}; pem={pem[:120]!r}")
    assert _RSA_ENCRYPTION_OID in der, (
        "publicKeyPem DER does not contain the rsaEncryption OID "
        f"(1.2.840.113549.1.1.1) — not a well-formed RSA SubjectPublicKeyInfo. "
        f"pem={pem[:120]!r}"
    )


def _outbox_doc(actor_url: str) -> tuple[int, dict | None, str]:
    return _get_json(f"{actor_url}/outbox", accept=AP_ACCEPT)


def _note_status(note_url: str) -> tuple[int, dict | None, str]:
    return _get_json(note_url, accept=AP_ACCEPT)


def _note_becomes_fetchable(note_url: str, timeout: float = 60.0) -> dict:
    def predicate():
        status, doc, raw = _note_status(note_url)
        return status == 200 and doc is not None, f"status={status} body={raw[:250]!r}"

    _poll_until(predicate, f"note {note_url} becomes fetchable (200)", timeout=timeout)
    status, doc, raw = _note_status(note_url)
    assert status == 200 and doc, (
        f"note fetch failed right after the poll observed 200: status={status} "
        f"body={raw[:300]!r}"
    )
    return doc


def _note_becomes_404(note_url: str, timeout: float = 60.0) -> None:
    def predicate():
        status, _doc, raw = _note_status(note_url)
        return status == 404, f"status={status} body={raw[:250]!r}"

    _poll_until(predicate, f"deleted note {note_url} becomes 404", timeout=timeout)


def _assert_no_ap_accounts(context: str) -> None:
    """The handle-INDEPENDENT half of the hard precondition, and the strong
    one: zero AP-enabled accounts on the whole box means no handle can resolve,
    so this runs first — before sign-in, before anything.

    NEVER a repair target (testing.md § shared-box rule, non-destructive
    carve-out). Unlinking a real pre-existing AP account to "normalize" the box
    would destroy real follower rows — user data this test has no authority to
    touch. A failure here means STOP, never "fix and continue".
    """
    total, ni_desc = _nodeinfo_users_total()
    if total != 0:
        pytest.fail(
            f"[{context}] refusing to touch a box with pre-existing "
            f"ActivityPub state: {ni_desc}. Unlinking a real account to "
            "normalize this would destroy real follower rows this test has "
            "no right to touch — it refuses to run instead of repairing."
        )


def _assert_handle_unfingerable(context: str, handle: str) -> None:
    """The per-handle half, run once the handle is known (i.e. after sign-in).

    Belt-and-braces on top of ``_assert_no_ap_accounts``: with zero AP accounts
    box-wide this cannot fail on its own, so a failure here means the two reads
    disagree — which is itself a reason to stop rather than proceed.
    """
    wf_status, _wf_doc, wf_raw = _webfinger(handle)
    if wf_status != 404:
        pytest.fail(
            f"[{context}] refusing to touch a box where acct:{handle} "
            f"already WebFingers (status={wf_status}, expected 404): "
            f"body={wf_raw[:300]!r}. A pre-existing AP account for this "
            "handle means real follower state this test must not disturb."
        )


# ── bridges-page precondition peek (UI-only, never a nest API) ─────────────


def _open_ap_bridge_detail(app) -> None:
    """linux: open the ActivityPub row's detail pane — the same one-line
    click ``BridgesActions._open_bridge`` performs — WITHOUT committing to
    either the link or unlink action, so the button's current label can be
    read as a pure precondition check."""
    app.bridges.navigate()
    app.driver.click("activitypub", wait_for_child="bridge-action-button")
    app.driver.wait_for("bridge-action-button")


def _bridge_action_label(app) -> str:
    return app.driver.get_text("bridge-action-button")


# ── the test ────────────────────────────────────────────────────────────────


@pytest.mark.feature("fediverse")
def test_activitypub_live_serving_surface(app):
    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the live ActivityPub serving-surface + bridges-page drive",
            detail="drives the linux Fauna app UI + bridges page; the "
            "other apps are the remaining cross-app follow-on",
            tracked="docs/goal/behavior/activitypub.md",
        )

    # -1) Preflight: is the box even set up for this run? A box that does not
    #    know the admin seed (unclaimed / reset / the wrong box) skips as
    #    environment instead of failing the serving assertions below, which would
    #    accuse working product code. Skipped when a claim code is supplied: then
    #    an unclaimed box is the expected start and the wizard claims it.
    if not CLAIM_CODE:
        live_box_door.preflight_admin(URL, SECRET, SECRET_SOURCE)

    # 0) Hard precondition, before touching ANYTHING (including sign-in): the
    #    box must show zero AP-enabled accounts. Handle-independent, and the
    #    strong half — zero accounts means nothing can WebFinger. Never "fixed"
    #    by unlinking; see the module docstring's rotation-hazard rationale.
    _assert_no_ap_accounts("pre-sign-in")

    # 1) Reach the admin shell purely through the wizard (claim or sign in).
    state = reach_admin_shell(app, nest_url=URL, secret_hex=SECRET, claim_code=CLAIM_CODE)
    print(f"\n[ap-live] reached admin shell via: {state}")
    wait_connected(app)

    # 1b) Derive the admin handle from the identity we just signed in as —
    #     the nest's own answer, not out-of-band env knowledge (module
    #     docstring; helpers/live_handle.py). Username derivation is
    #     handle-verbatim at first enablement, so the bare localpart IS the
    #     AP username (no collision possible under the zero-accounts
    #     precondition) — activitypub.md § Identity & keys -> Username
    #     derivation.
    handle, username = derive_handle(app, URL)
    print(f"[ap-live] derived handle from the secret: acct:{handle} (username {username!r})")

    # 1c) The per-handle half of the precondition, now that the handle is
    #     known. Still before any mutation — sign-in mutated nothing.
    _assert_handle_unfingerable("post-sign-in", handle)

    marker = uuid.uuid4().hex
    marker_text = f"ActivityPub live serving-surface e2e probe {marker}"
    linked = False
    post_pending_cleanup = False

    try:
        # 2) Belt-and-braces UI precondition: peek the bridge's action-button
        #    label WITHOUT committing to link or unlink. Same rationale as
        #    step 0 — a race between the two checks (or a stale nodeinfo/
        #    webfinger read) must still be caught before any mutation.
        _open_ap_bridge_detail(app)
        label = _bridge_action_label(app)
        if label == S.bridges.unlink_bridge:
            pytest.fail(
                "refusing to run: the ActivityPub bridge already reads "
                f"{label!r} (linked) even though the public nodeinfo/"
                "webfinger precondition just passed — never unlinking to "
                "'fix' this, see module docstring.\n"
                + dump(app, ("bridge-action-button", "page-heading", "error-message"))
            )
        assert label == S.bridges.link_bridge, (
            f"expected the ActivityPub action button to read "
            f"{S.bridges.link_bridge!r} on an unlinked account, got "
            f"{label!r}.\n" + dump(app, ("bridge-action-button", "page-heading"))
        )

        # 3) Link (zero-field `enable` mode — activitypub.md § Control plane).
        app.bridges.link("activitypub")
        linked = True

        # 4) Enable asserts (poll each — the box is remote).
        _users_total_reaches(1)

        wf_doc = _webfinger_resolves(handle)
        assert wf_doc.get("subject") == f"acct:{handle}", (
            f"webfinger subject mismatch: {wf_doc!r}"
        )
        actor_url = _extract_self_actor_url(wf_doc)

        actor_status, actor_doc, actor_raw, actor_ct = _actor_doc(actor_url)
        assert actor_status == 200, (
            f"actor doc GET {actor_url} failed: status={actor_status} "
            f"body={actor_raw[:300]!r}"
        )
        assert "activity+json" in actor_ct, (
            f"actor doc Content-Type should carry activity+json, got {actor_ct!r}"
        )
        assert actor_doc.get("preferredUsername") == username, (
            f"preferredUsername should be the handle-verbatim username "
            f"{username!r}: {actor_doc!r}"
        )
        assert actor_doc.get("id") == actor_url, (
            f"actor id should equal its own WebFinger-resolved URL: "
            f"id={actor_doc.get('id')!r} actor_url={actor_url!r}"
        )
        _assert_rsa_public_key_pem((actor_doc.get("publicKey") or {}).get("publicKeyPem", ""))

        outbox_status, outbox_doc, outbox_raw = _outbox_doc(actor_url)
        assert outbox_status == 200, (
            f"outbox GET {actor_url}/outbox failed: status={outbox_status} "
            f"body={outbox_raw[:300]!r}"
        )
        assert outbox_doc.get("type") == "OrderedCollection", (
            f"outbox root should be an OrderedCollection: {outbox_doc!r}"
        )
        # Accounts enable with backfill=0 (ap_accounts schema default) — an
        # EMPTY outbox here is the designed behavior, not a bug: bulk
        # history enumeration is opt-in, while a pushed note's own id must
        # stay individually fetchable regardless (asserted below).
        assert outbox_doc.get("totalItems") == 0, (
            f"a fresh backfill=0 account's outbox should report 0 items: {outbox_doc!r}"
        )

        # 5) Post through the real feed composer, then find OUR post by
        #    marker (the human's real posts are in this feed too — never
        #    assume index 0).
        app.driver.navigate_to("feed")
        app.feed.create_post(marker_text)
        post_pending_cleanup = True

        posts = app.feed._feed_posts_from_state()
        ours = next((p for p in posts if marker in p.get("body", "")), None)
        assert ours is not None, (
            f"our post (marker {marker!r}) not found in feed state: {posts!r}"
        )
        post_id_hex = ours["post_id"]

        note_url = f"{actor_url}/notes/{post_id_hex}"
        note_doc = _note_becomes_fetchable(note_url)
        assert note_doc.get("type") == "Note", f"expected a Note: {note_doc!r}"
        assert note_doc.get("id") == note_url, (
            f"note id should equal its own dereference URL: "
            f"id={note_doc.get('id')!r} note_url={note_url!r}"
        )
        assert note_doc.get("attributedTo") == actor_url, (
            f"note attributedTo should be our actor: {note_doc!r}"
        )
        assert marker in note_doc.get("content", ""), (
            f"note content should carry the marker text: {note_doc!r}"
        )

        # Backfill gate re-check: the pushed note stays fetchable by id
        # while bulk outbox enumeration stays off — not a race, the two are
        # independently gated (get_note is deliberately NOT gated on
        # `backfill`; get_outbox is).
        outbox_status2, outbox_doc2, outbox_raw2 = _outbox_doc(actor_url)
        assert outbox_status2 == 200 and outbox_doc2.get("totalItems") == 0, (
            f"outbox should STILL report 0 items after a note became "
            f"individually fetchable (the backfill gate, not a bug): "
            f"status={outbox_status2} body={outbox_raw2[:300]!r}"
        )

        # 6) Delete through the real feed UI, then confirm the servability
        #    filter makes the note indistinguishable from never-existed
        #    (a plain 404 — this nest does not serve AP Tombstone objects;
        #    that is the designed shape, not a partial implementation).
        posts_now = app.feed._feed_posts_from_state()
        idx = next(
            (i for i, p in enumerate(posts_now) if marker in p.get("body", "")), None
        )
        assert idx is not None, (
            f"our post (marker {marker!r}) vanished from feed state before "
            f"we could delete it: {posts_now!r}"
        )
        app.feed.delete_post(idx)
        post_pending_cleanup = False

        _note_becomes_404(note_url)

    finally:
        # 7) Teardown — always attempt, regardless of where the test failed,
        #    so a mid-test assertion failure never strands the shared box in
        #    a linked state (activitypub.md § Architecture; testing.md §
        #    shared-box rule).
        if post_pending_cleanup:
            try:
                leftover = app.feed._feed_posts_from_state()
                idx = next(
                    (i for i, p in enumerate(leftover) if marker in p.get("body", "")),
                    None,
                )
                if idx is not None:
                    app.feed.delete_post(idx)
            except Exception as e:  # noqa: BLE001 — best-effort teardown
                print(f"[ap-live] teardown: failed to delete leftover test post: {e}")

        if linked:
            try:
                app.bridges.navigate()
                app.bridges.unlink("activitypub")
            except Exception as e:  # noqa: BLE001 — still verify below regardless
                print(f"[ap-live] teardown: unlink action raised: {e}")

            _users_total_reaches(0, timeout=90.0)
            _webfinger_404s(handle, timeout=90.0)
            print(f"[ap-live] teardown complete: acct:{handle} unlinked, username freed")
