"""Build + start a fauna-nest binary with ActivityPub enabled — shared e2e infra.

Both AP e2e surfaces stand up the same locally-built nest binary:

  * the two-nest / fake-fediverse federation suite (`tests/api/test_activitypub_federation.py`),
    which binds loopback and talks to in-process peers, and
  * the real-fediverse interop harness (`tests/platform/fediverse/`), which binds
    `0.0.0.0` so a container can reach it through host-gateway, and injects the
    D4 resolve-override + extra-CA `test-hooks` envs.

Both build `fauna-nest` with `--features activitypub,test-hooks`: every AP peer
in the harness lives behind loopback (the federation peers directly; Mastodon's
TLS front is loopback-published at `127.0.0.1:8443`), and AP's outbound dials are
SSRF-guarded (`activitypub/outbound.rs`) — loopback is refused unless the binary
was built with `test-hooks` AND `FAUNA_TEST_AP_ALLOW_LOOPBACK` is set. The safety
argument rests on `test-hooks` alone: the nest Docker image DOES ship
`activitypub` (`--features bluesky,nostr,activitypub`)
and builds no `test-hooks`, so nothing here loosens the shipping nest. Corrected
2026-08-06 — this said "production builds neither feature", which is false of
`activitypub` and made the guard's real hinge easy to misread.
"""

from __future__ import annotations

import os
import sqlite3
import subprocess

from common import (
    CLAIM_CODE,
    build_node,
    claim_admin,
    mark_tls_nest,
    nest_id_from_data_dir,
    wait_for_node,
)
from drivers.port_util import popen_group_kwargs, reap_descendants_of

# The provider id on the unified `fauna.bridges.*` control plane — the only
# ActivityPub control surface (`activitypub.md` § Control plane).
AP_BRIDGE_ID = "activitypub"


def enable_ap(nest, user) -> tuple[str, str]:
    """Link the ActivityPub bridge for `user`; return `(username, actor_url)`.

    The control plane is `fauna.bridges.link` (the `/api/v1/activitypub/*` HTTP
    twin was ripped). The username is the last segment of the actor URL.
    """
    from tests.api import ws_api

    reply = ws_api.bridge_link(nest["port"], user, AP_BRIDGE_ID, "enable")
    assert reply["linked"] is True, f"link failed: {reply}"
    actor_url = reply["identity"]["value"]
    return actor_url.rsplit("/", 1)[-1], actor_url


def build_ap_nest_binary() -> str:
    """Build fauna-nest with the activitypub + test-hooks features; return its path.

    `test-hooks` rides along because every AP peer in the e2e suites lives behind
    loopback and AP's outbound SSRF guard refuses loopback without it (see the
    module docstring). Production builds neither feature.

    Built by the shared `build_node` — under a machine-wide `build` slot, warm
    runs taking none, the returned path a per-variant pinned copy — never by a
    bare `cargo build` here. The conftest
    prebuild warms it for `ap_binary` outside every test's timeout.
    """
    return build_node(features="activitypub,test-hooks")


def start_ap_nest(binary, tmp_dir, port, domain, *, bind_host="127.0.0.1",
                  extra_env=None, serve_tls=False):
    """Start a nest instance with AP enabled.

    ``bind_host`` selects the listen interface: the federation suite keeps the
    default loopback; the Mastodon harness passes ``"0.0.0.0"`` so the container
    can reach the nest across the host boundary (loopback is unreachable from a
    container). Host-side callers always reach it back through ``127.0.0.1`` —
    the returned ``url`` — which a ``0.0.0.0`` bind still serves.

    ``serve_tls`` makes the nest serve its always-live self-signed **floor** cert
    over real HTTPS on its API listener, and marks the port so host-side dials
    (``ws_api``, ``common.auth``, ``wait_for_node``) target ``https://`` +
    CERT_NONE. This is **required** for a public AP domain like ``nest.test``: the
    boot guard ``refuse_plain_http_for_public_domain`` refuses to serve the API in
    cleartext when a public domain is configured, and the nest owns TLS
    end-to-end even in production (the SNI router only PROXY-passes). We (a) drop
    the process-wide ``FAUNA_INSECURE_DISABLE_TLS`` escape for this child so it
    binds TLS, and (b) point ``--acme-dir`` at a per-run writable dir so
    ``listener_tls_from_floor`` can write+serve the floor cert (the default
    ``/var/lib/fauna/acme`` is unwritable). We deliberately add **no** ``[acme]``
    section, so ``acme_http01_mode`` stays off — no HTTP-01 challenge listener,
    no cert-lifecycle task, and no Let's Encrypt contact for the un-orderable
    ``nest.test`` (the floor is served regardless). The federation suite keeps the
    default plain-HTTP posture (its ``ip:port`` domain is not a public DNS name,
    so cleartext is allowed).

    ``extra_env`` is merged into the child's environment on top of the always-set
    ``FAUNA_TEST_AP_ALLOW_LOOPBACK=1``; the Mastodon harness uses it to pass the
    D4 resolve-override (``FAUNA_TEST_AP_RESOLVE_JSON``) and extra-CA
    (``FAUNA_TEST_AP_EXTRA_CA_PEM``) hooks.
    """
    tmp_dir = str(tmp_dir)
    os.makedirs(tmp_dir, exist_ok=True)
    db_path = os.path.join(tmp_dir, "nest.db")
    scheme = "https" if serve_tls else "http"

    config = f"""[nest]
mode = "public"
listen = "{bind_host}:{port}"
db_path = "{db_path}"
domain = "{domain}"
require_registration = false
registration_mode = "open"
"""
    config_path = os.path.join(tmp_dir, "config.toml")
    with open(config_path, "w") as f:
        f.write(config)

    # Write claim code file
    claim_code_path = os.path.join(tmp_dir, "claim-code")
    with open(claim_code_path, "w") as f:
        f.write(CLAIM_CODE)

    # Start server. `registration_mode = "open"` in the config backs
    # self-service registration — the handle-derived-username test needs a
    # handled actor; the admin-created actors every other test uses stay
    # handle-less (the legacy 16-hex derivation path). The registration
    # signature must be over the nest's identity domain, which boot seeds
    # from `[nest] domain` (`identity_domain_core::resolve_identity_domain`)
    # — i.e. this fixture's `domain`, not a `--handle-domain` value (the
    # identity domain outranks it).
    # Open the `test-hooks` loopback exemption in AP's SSRF guard (see
    # `build_ap_nest_binary`). Only loopback is exempted — a private-range or
    # cloud-metadata target stays rejected even here.
    ap_env = {**os.environ, "FAUNA_TEST_AP_ALLOW_LOOPBACK": "1", **(extra_env or {})}
    argv = [binary, "--bind", f"{bind_host}:{port}", "--db", db_path, "--config", config_path]
    if serve_tls:
        # Mark the port BEFORE any host-side dial so wait_for_node/claim_admin/
        # ws_api all resolve https:// (+ CERT_NONE) for the floor cert.
        mark_tls_nest(port)
        acme_dir = os.path.join(tmp_dir, "acme")
        os.makedirs(acme_dir, exist_ok=True)
        argv += ["--acme-dir", acme_dir]
        # Drop the process-wide plain-HTTP escape for THIS child so it binds TLS
        # (a public domain + no TLS would make it refuse to boot).
        ap_env.pop("FAUNA_INSECURE_DISABLE_TLS", None)
    # Both streams go to a per-run FILE, never a pipe. Two reasons, and the
    # second is not cosmetic: (a) the nest's own log is the only witness for
    # server-side failures a black-box client read cannot see — an AP inbox
    # rejecting an activity answers the *peer*, not us (`nest_log` below); and
    # (b) an unread `subprocess.PIPE` is a wedge waiting to happen — once the
    # nest writes ~64 KB the pipe buffer fills and the nest BLOCKS on its next
    # log line, which reads as a hang with no diagnosis at all.
    log_path = os.path.join(tmp_dir, "nest.log")
    log_file = open(log_path, "wb")
    # Armed against point 9's die-with-the-run guarantee (both halves; the Windows
    # half is a no-op off Windows). A nest is the long-lived, grandchild-owning
    # case the guarantee exists for: unarmed, a SIGKILLed pytest leaves it holding
    # its port and data dir against the next run. Unarmed until 2026-08-14 —
    # invisible to the structural pin, which never looks at a file that has not
    # already learned half the convention. This site is also the red-verify
    # specimen for that blindness (see check_spawn_arming_ratchet.py's docstring).
    proc = subprocess.Popen(
        argv,
        stdout=log_file, stderr=subprocess.STDOUT,
        env=ap_env,
        **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    log_file.close()  # the child holds its own dup of the fd
    wait_for_node(port, scheme=scheme)

    # Claim admin via claim code
    admin = claim_admin(port, CLAIM_CODE)

    # The nest's identity (== `nest_id`) from its durable deployment-seed file —
    # `FederationChannelClient` needs it on both the initiator (hello signer) and
    # the target (hello binding). The reader is `common.nest`'s, not a fourth
    # hand-copy of it: this used to say "mirroring start_nest", which is exactly
    # the shape that half-migrates when the nest renames the file.
    nest_id = nest_id_from_data_dir(tmp_dir)

    return {
        "proc": proc,
        "port": port,
        "url": f"{scheme}://127.0.0.1:{port}",
        "domain": domain,
        "db_path": db_path,
        "tmp_dir": tmp_dir,
        "nest_id": nest_id,
        "admin": admin,
        "log_path": log_path,
    }


def nest_log(nest) -> str:
    """The nest's captured stdout+stderr so far.

    The inbound half of AP federation fails *server-side*: a rejected activity
    gets an HTTP status the remote sees and we do not, so "the follow never
    arrived" and "the follow arrived and we refused it" are indistinguishable
    from any client-side read. The nest's own `tracing` output is the witness
    that tells them apart — assert on it rather than inferring a cause from a
    downstream absence.
    """
    try:
        with open(nest["log_path"], "r", errors="replace") as f:
            return f.read()
    except FileNotFoundError:
        return ""


# ── nest-internal state reads (`nest.db`, read-only) ─────────────────────────
#
# The inbound half of AP interop is asserted nest-side, because the flows have
# no Mastodon-visible result to read back: an ingested reply and the synthetic
# upvote/repost a Like/Announce mints are *our* rows. Two reasons a nest API
# read cannot stand in for them:
#
#   * bridge-ingested posts are deliberately NOT FTS-indexed
#     (`CacheDb::put_post_with_source_index_only`), so `fauna.search.query`
#     never returns them; and
#   * the synthetic reaction posts deliberately do not bump engagement counts
#     (the AP mint path calls `store_post` directly, never
#     `record_reference_engagements`), so no counter moves either.
#
# So we read the projection at rest, mirroring `helpers/segment_at_rest.py`.


def _read_db(nest):
    """Open the nest's `nest.db` read-only (never mutate a live nest's state)."""
    return sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True, timeout=10.0)


def reaction_map_key(verb: str, actor_uri: str, object_url: str) -> str:
    """The `ap_post_map.ap_url` key a minted synthetic reaction is filed under.

    Mirrors `ReactionVerb::map_key` in `activitypub/inbox_routes.rs`: the key is
    `{verb}:{actor}:{object}`, deliberately NOT the activity id — that is what
    makes a replayed Like idempotent and gives `Undo` a stable lookup.
    """
    assert verb in ("like", "announce"), f"unknown reaction verb {verb!r}"
    return f"{verb}:{actor_uri}:{object_url}"


def ap_post_map_row(nest, ap_url: str) -> dict | None:
    """One `ap_post_map` row by its `ap_url` key, or None.

    For an ingested remote note the key is the note's AP URL; for a synthetic
    reaction it is the `reaction_map_key` above.
    """
    conn = _read_db(nest)
    try:
        row = conn.execute(
            "SELECT fauna_post_id, actor_id, created_at, tombstoned, remote_actor_uri "
            "FROM ap_post_map WHERE ap_url = ?",
            (ap_url,),
        ).fetchone()
    finally:
        conn.close()
    if row is None:
        return None
    return {
        "fauna_post_id": row[0],
        "actor_id": row[1],
        "created_at": row[2],
        "tombstoned": bool(row[3]),
        "remote_actor_uri": row[4],
    }


def local_note_url(nest, post_id_hex: str) -> str | None:
    """The AP URL the Create-push published a local post under, or None.

    The `ap_post_map` row the push writes is what makes an inbound reaction on
    our own note resolvable — it is the object URL a remote peer reacts to.
    """
    conn = _read_db(nest)
    try:
        row = conn.execute(
            "SELECT ap_url FROM ap_post_map WHERE fauna_post_id = ? AND tombstoned = 0",
            (post_id_hex,),
        ).fetchone()
    finally:
        conn.close()
    return row[0] if row else None


def reaction_rows_for_object(nest, verb: str, object_url: str) -> list[dict]:
    """Every synthetic-reaction map row of `verb` filed against `object_url`.

    Matched by key *shape* rather than by a composed key, so a caller need not
    know the reacting actor's AP URI (Mastodon's actor-id format is its own
    business — design rider: we never read Mastodon source to learn it).
    """
    assert verb in ("like", "announce"), f"unknown reaction verb {verb!r}"
    prefix, suffix = f"{verb}:", f":{object_url}"
    conn = _read_db(nest)
    try:
        rows = conn.execute(
            "SELECT ap_url, fauna_post_id, actor_id, tombstoned, remote_actor_uri "
            "FROM ap_post_map"
        ).fetchall()
    finally:
        conn.close()
    return [
        {
            "ap_url": r[0],
            "fauna_post_id": r[1],
            "actor_id": r[2],
            "tombstoned": bool(r[3]),
            "remote_actor_uri": r[4],
        }
        for r in rows
        if r[0].startswith(prefix) and r[0].endswith(suffix)
    ]


def content_row_exists(nest, post_id_hex: str) -> bool:
    """True while the post's `content` projection row is present.

    The withdrawal half of the `Undo` retraction: `retract_reaction` tombstones
    the map row *and* deletes the synthetic post's projection
    (`delete_post_projection`), so a retracted reaction leaves neither.
    """
    conn = _read_db(nest)
    try:
        row = conn.execute(
            "SELECT 1 FROM content WHERE id = ?", (bytes.fromhex(post_id_hex),)
        ).fetchone()
    finally:
        conn.close()
    return row is not None


def delivery_jobs_for(nest, inbox_url: str) -> list[str]:
    """Every activity the nest ever queued for `inbox_url`, oldest first, as
    the JSON text it queued — whatever the job's status.

    `ap_delivery_queue` rows are never deleted (a delivered job is marked
    `done`, a dead one `failed`), so this is the full record of what the nest
    decided to send one inbox. That makes a *negative* delivery claim
    assertable without waiting out a clock: once a fan-out is known complete,
    an inbox with no job was never a target.
    """
    conn = _read_db(nest)
    try:
        rows = conn.execute(
            "SELECT activity_json FROM ap_delivery_queue "
            "WHERE target_inbox = ? ORDER BY id",
            (inbox_url,),
        ).fetchall()
    finally:
        conn.close()
    return [r[0] for r in rows]
