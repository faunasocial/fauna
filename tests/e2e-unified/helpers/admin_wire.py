"""Admin-class WS-RPC seeds and ground-truth reads shared by the admin picker
journeys (the admin-web front-page picker, the admin-dns catch-all and
role-address pickers, the admin-users guardian picker).

Fixture setup and read-back only: every mutation under test goes through the app
UI (e2e-conventions.md convention 8). One home, so each journey reads the
authoritative wire surface the same way — the account list a picker offers and
the designation a pick persisted.
"""

from clients.ws_rpc_admin_client import WsRpcAdminClient


def admin_client(nest) -> WsRpcAdminClient:
    """An Admin-class WS-RPC connection authenticated as ``nest``'s admin."""
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def admin_user(nest) -> dict:
    """``nest``'s admin as a ``fauna.admin.users.list`` row, found across EVERY
    page (``WsRpcAdminClient.users_list_all``) — the same account list every
    admin actor picker offers.

    One page is not enough: the list is newest first, so the admin — the box
    claimer, the oldest account — falls off page one once the nest holds more
    accounts than a page, and the shared session nest does well before a full
    suite ends. Selected by ``is_admin``, not position: a session-scoped
    mail-bridge fixture inserts a blank-label bridge-service-user row that sorts
    ahead of it."""
    with admin_client(nest) as client:
        users = client.users_list_all()
    assert users, "no accounts in fauna.admin.users.list"
    admin = next((u for u in users if u.get("is_admin")), None)
    assert admin is not None, (
        f"no admin actor among all {len(users)} fauna.admin.users.list accounts"
    )
    return admin


def picker_option(user: dict) -> str:
    """The option text every admin actor picker paints for ``user``: its handle,
    else its full actor hex — ``fauna_client_admin::admin_picker_option``
    (``admin.md`` § 2 → *What identifies a user in an admin picker*)."""
    return user.get("handle") or bytes(user["actor_id"]).hex()


def seed_local_domain(nest, domain: str) -> None:
    """Add a hosted mail domain — the row the admin-dns pickers render on."""
    with admin_client(nest) as client:
        client.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": domain,
                "mta_sts_cert_mode": "expand_primary",
            },
        )


def _local_domain_row(nest, domain: str) -> dict | None:
    with admin_client(nest) as client:
        reply = client.call("fauna.bridges.list_local_domains", {})
    return next(
        (row for row in reply.get("active", []) if row.get("domain_name") == domain),
        None,
    )


def catch_all_actor_for(nest, domain: str):
    """``domain``'s persisted ``catch_all_actor_id`` (bytes) or ``None``, read back
    over ``fauna.bridges.list_local_domains`` — the authoritative designation."""
    row = _local_domain_row(nest, domain)
    return row.get("catch_all_actor_id") if row else None


def role_overrides_for(nest, domain: str) -> dict:
    """``domain``'s persisted ``role_address_overrides`` map (``{role: actor-hex}``),
    read typed off ``fauna.bridges.list_local_domains`` (the wire row carries the
    map itself, not its JSON text). A domain carrying none yields ``{}``."""
    row = _local_domain_row(nest, domain)
    raw = (row.get("role_address_overrides") if row else None) or {}
    return {role: actor for role, actor in raw.items() if actor}


def dmarc_policy_for(nest, domain: str) -> dict:
    """``domain``'s DMARC policy as the nest holds and publishes it
    (``dmarc-reporting.md`` § Multi-domain deployments): ``stored`` is the
    override's ``policy_mode`` off ``fauna.bridges.list_local_domains`` (``None``
    when the domain runs the default — choosing ``reject`` clears it), and
    ``published`` is the ``(p, sp)`` pair of the ``_dmarc`` record body
    ``fauna.dns.list_records`` assembles for it."""
    row = _local_domain_row(nest, domain)
    overrides = (row.get("dmarc_overrides") if row else None) or {}
    dmarc = [
        r
        for r in dns_matrix_for(nest, domain)
        if r["record_type"] == "TXT" and r["name"].rstrip(".") == f"_dmarc.{domain}"
    ]
    assert len(dmarc) == 1, f"expected one _dmarc record for {domain!r}: {dmarc!r}"
    tags = dict(
        part.strip().split("=", 1) for part in dmarc[0]["expected"].split(";") if "=" in part
    )
    return {
        "stored": overrides.get("policy_mode"),
        "published": (tags.get("p"), tags.get("sp")),
    }


def dns_matrix_for(nest, domain: str) -> list[dict]:
    """``domain``'s expected record matrix over ``fauna.dns.list_records`` — one
    ``{name, record_type, expected, ttl_seconds}`` row per record, the values the
    admin-dns page must list (``dns-management.md`` § Manual + live
    verification)."""
    with admin_client(nest) as client:
        reply = client.call("fauna.dns.list_records", {"domain": domain})
    row = next((d for d in reply.get("domains", []) if d.get("domain") == domain), None)
    assert row is not None, f"fauna.dns.list_records has no matrix for {domain!r}"
    return row.get("records", [])


def dns_matrix_domain_count(nest) -> int:
    """How many domains the whole ``fauna.dns.list_records`` matrix lists — the
    number an app's matrix-load log line reports (``fauna_client_dns``'s
    ``MATRIX_LOAD_LOG_LINE``)."""
    with admin_client(nest) as client:
        reply = client.call("fauna.dns.list_records", {"domain": None})
    return len(reply.get("domains", []))


def dns_verdicts_for(nest, domain: str) -> dict:
    """``domain``'s live public-DNS verdicts over ``fauna.dns.verify_records``,
    keyed ``(name, record_type)`` → the wire status (``ok`` / ``missing`` /
    ``mismatch`` / ``checking``)."""
    with admin_client(nest) as client:
        reply = client.call("fauna.dns.verify_records", {"domain": domain})
    row = next((d for d in reply.get("domains", []) if d.get("domain") == domain), None)
    assert row is not None, f"fauna.dns.verify_records has no verdicts for {domain!r}"
    return {(r["name"], r["record_type"]): r["status"] for r in row.get("records", [])}


def apex_actor(nest):
    """The persisted front-page (apex) actor id (bytes) or ``None``, read over
    ``fauna.web.get_apex_actor`` — the authoritative designation."""
    with admin_client(nest) as client:
        return client.call("fauna.web.get_apex_actor", {}).get("actor_id")


def invite_code_guardian(nest, code: str):
    """The guardian actor id (bytes) invite ``code`` admits under, or ``None`` for
    an unsupervised code — ``fauna.admin.invite_codes.list``'s ``guardian_actor``.

    A code missing from the list fails here rather than reading as "no
    guardian", so a mint that never landed cannot pass a guardian assertion."""
    with admin_client(nest) as client:
        codes = client.call("fauna.admin.invite_codes.list", {}).get("invite_codes", [])
    row = next((c for c in codes if c.get("code") == code), None)
    assert row is not None, (
        f"invite code {code!r} is not in fauna.admin.invite_codes.list: "
        f"{[c.get('code') for c in codes]!r}"
    )
    return row.get("guardian_actor")


def invite_code_age_band(nest, code: str):
    """The age band (wire token) invite ``code`` admits under, or ``None`` for a
    code minted without one — ``fauna.admin.invite_codes.list``'s ``age_band``
    (family-safety.md § The account age band). Same missing-code strictness as
    `invite_code_guardian`."""
    with admin_client(nest) as client:
        codes = client.call("fauna.admin.invite_codes.list", {}).get("invite_codes", [])
    row = next((c for c in codes if c.get("code") == code), None)
    assert row is not None, (
        f"invite code {code!r} is not in fauna.admin.invite_codes.list: "
        f"{[c.get('code') for c in codes]!r}"
    )
    return row.get("age_band")


def admit_adult(nest, handle: str, *, domain: str | None = None, token: bool = False) -> dict:
    """Direct-admit an ordinary (unsupervised) account under ``handle`` over the
    admin-authenticated ``fauna.admin.users.create`` -- the third
    account-creation path (``public-mode.md`` § Registration & Identity: the
    ceremony, the admin claim, or an admin admitting a user directly).

    **The seat-seeding door for every account whose admission is not under
    test** (``testing.md`` § Default app and nest mode, the shipped-limit fit
    rule): it spends neither the anonymous ``fauna.account.register`` budget
    nor the ``invite_request.submit`` one. Under ``--nest docker`` every
    anonymous call arrives from the docker gateway address, so a per-source
    budget is a budget per RUN, and a module seeding its seats through the
    anonymous register door sheds ``gate=register`` there. Keep that door
    (``common.auth.register_handled_actor``) only for a test whose subject IS
    self-service registration.

    It cannot mint a WARD: supervision is set at admission and this helper
    admits unsupervised -- ``admit_ward_directly`` is the supervised arm.
    ``label=handle`` restores the production shape (``create_user_with_handle``
    defaults a handle-registered user's label to its handle), so the account is
    distinct in every admin picker.

    ``domain`` (optional) is the handle domain the caller would have signed an
    anonymous register over. It is checked against the nest's own resolved
    domains (``fauna.nest.info``'s ``registration.handle_domain`` and identity
    ``domain``, read once per nest over the admin connection), so a caller
    relying on ``<handle>@<domain>`` fails here exactly where the anonymous
    register would have refused the signature. The handle itself is stored bare
    and globally unique on both doors.

    ``token=True`` also mints and holds the account's bearer
    (``common.auth.mint_and_hold_token``), so the dict is shaped like
    ``register_handled_actor``'s -- a drop-in for seat seeding.

    Returns ``{"signing_key", "actor_id_hex", "actor_id_bytes", "handle",
    "registered_domain"}`` plus ``"token"`` when asked. ``registered_domain`` is
    the deployment's identity domain (what ``RegisterReply.domain`` carries),
    read only when ``domain`` or ``token`` is passed, else ``None``. The
    direct-admission UI is `test_admin_users_admit.py`'s subject, not this
    helper's."""
    from nacl.signing import SigningKey

    from common.auth import mint_and_hold_token, register_user

    resolved = None
    if domain is not None or token:
        resolved = _nest_identity_domains(nest)
        if domain is not None:
            assert domain in (resolved["handle_domain"], resolved["domain"]), (
                f"admit_adult({handle!r}, domain={domain!r}): the nest's resolved "
                f"handle domain is {resolved['handle_domain']!r} (identity "
                f"{resolved['domain']!r}); an anonymous register signed over "
                f"{domain!r} would have been refused as signature_failed"
            )
    sk = SigningKey.generate()
    actor_bytes = bytes(sk.verify_key)
    actor_hex = actor_bytes.hex()
    register_user(
        nest["port"],
        actor_hex,
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
        handle=handle,
        label=handle,
    )
    out = {
        "signing_key": sk,
        "actor_id_hex": actor_hex,
        "actor_id_bytes": actor_bytes,
        "handle": handle,
        "registered_domain": resolved["domain"] if resolved else None,
    }
    if token:
        out["token"] = mint_and_hold_token(nest["url"].rstrip("/"), sk)
    return out


_IDENTITY_DOMAINS: dict = {}


def _nest_identity_domains(nest) -> dict:
    """``{"domain", "handle_domain"}`` of ``nest`` from ``fauna.nest.info``, read
    over the admin's cached authenticated connection (an authenticated caller's
    throttle bucket is its actor, never the docker gateway's address) and
    cached per (base url, admin actor) -- one read per nest, so seat seeding
    adds no per-seat directory read. A test that renames the primary domain
    keeps the anonymous register door, which reads the live reply."""
    from common.auth import _authed_call

    admin_sk = nest["admin"]["signing_key"]
    key = (nest["url"].rstrip("/"), bytes(admin_sk.verify_key).hex())
    if key not in _IDENTITY_DOMAINS:
        info = _authed_call(nest["url"].rstrip("/"), admin_sk, "fauna.nest.info", {})
        _IDENTITY_DOMAINS[key] = {
            "domain": info.get("domain"),
            "handle_domain": (info.get("registration") or {}).get("handle_domain"),
        }
    return _IDENTITY_DOMAINS[key]


def admit_ward_directly(nest, guardian_hex: str, handle: str) -> dict:
    """Direct-admit a SUPERVISED account under ``guardian_hex`` over
    ``fauna.admin.users.create`` with its ``guardian_actor`` (the
    direct-admission arm of `family-safety.md` § The guardianship link) — fixture
    setup for a test that needs a guardianship link to stand, costing no
    anonymous submit. Returns ``{"signing_key", "actor_id_hex"}``."""
    from nacl.signing import SigningKey

    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    with admin_client(nest) as admin:
        admin.call(
            "fauna.admin.users.create",
            {
                "actor_id": bytes.fromhex(actor_hex),
                "tier": "free",
                "label": handle,
                "handle": handle,
                "guardian_actor": bytes.fromhex(guardian_hex),
            },
        )
    return {"signing_key": sk, "actor_id_hex": actor_hex}


def eviction_state(nest, actor_hex: str):
    """``actor_hex``'s eviction status as ``fauna.admin.evictions.list`` reports
    it (``"warning"``/``"suspended"``/``"deleting"``), or ``None`` when it has no
    eviction standing — the ground truth a row's controls are drawn from."""
    with admin_client(nest) as admin:
        rows = admin.call("fauna.admin.evictions.list", {})["evictions"]
    want = bytes.fromhex(actor_hex)
    row = next((r for r in rows if bytes(r["actor_id"]) == want), None)
    return ((row or {}).get("eviction") or {}).get("status")


def graduate_ward(nest, ward_hex: str) -> None:
    """Resolve a guardianship link from the admin side (`fauna.family.graduate`
    accepts the guardian OR the admin — the admin is how a cut-off guardian's
    links get resolved, `family-safety.md` § Lifecycle gates)."""
    with admin_client(nest) as admin:
        admin.call("fauna.family.graduate", {"supervised_actor_id": bytes.fromhex(ward_hex)})


def post_nest_hook(nest, path: str, body: dict) -> dict:
    """POST ``body`` to the nest's ``test-hooks`` endpoint ``/api/v1/test/<path>``
    (compiled into the e2e nest only, convention 15) and return its JSON."""
    import json
    import urllib.request

    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"test hook {path} returned {resp.status}"
        return json.loads(resp.read())


def run_due_pending_actions(nest) -> dict:
    """Make every pending action on ``nest`` due and run the production executor
    once — the ``test-hooks`` endpoint ``pending_actions/run_due``
    (``pending_actions_test_hook.rs``), never a wait (convention 14).

    NEST-WIDE: it runs every other test's pending action too, so ``nest`` must
    be a dedicated one, never the shared session nest."""
    return post_nest_hook(nest, "pending_actions/run_due", {})


def grant_admin(nest, member: dict) -> int:
    """Make ``member`` (``{"signing_key", "actor_id_hex"}``) a second admin of
    ``nest``, which must still have exactly one admin, its claimer: the claimer
    schedules ``fauna.admin.admins.add`` (a sole admin's grant needs no approval
    nobody could give) and `run_due_pending_actions` brings it due. Returns the
    executed action's id. A dedicated nest only (`run_due_pending_actions`)."""
    member_id = bytes.fromhex(member["actor_id_hex"])
    with admin_client(nest) as client:
        reply = client.call("fauna.admin.admins.add", {"actor_id": member_id})
    assert reply["status"] == "pending", reply
    action_id = reply["pending_action_id"]
    run_due_pending_actions(nest)
    with admin_client(nest) as client:
        actions = client.call("fauna.admin.pending_actions.list", {})["actions"]
        admins = {bytes(a["actor_id"]) for a in client.call("fauna.admin.admins.list", {})["admins"]}
    row = next((a for a in actions if a["id"] == action_id), None)
    assert row is not None and row["status"] == "executed", (
        f"the admin grant to {member['actor_id_hex']} did not run: {row!r}"
    )
    assert member_id in admins, f"{member['actor_id_hex']} is not among the admins after its grant"
    return action_id
