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


def admit_adult(nest, handle: str) -> dict:
    """Direct-admit an ordinary (unsupervised) account over
    ``fauna.admin.users.create`` — fixture setup for a test that needs a
    guardian to pick (the picker offers every non-suspended account). Returns
    ``{"signing_key", "actor_id_hex"}``. Costs no anonymous submit; the
    direct-admission UI is `test_admin_users_admit.py`'s subject, not this one."""
    from nacl.signing import SigningKey

    from common.auth import register_user

    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    register_user(
        nest["port"],
        actor_hex,
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
        handle=handle,
        label=handle,
    )
    return {"signing_key": sk, "actor_id_hex": actor_hex}
