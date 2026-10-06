"""tier_3 e2e for the user-tier alias WS-RPC surface (§ A2.1).

Drives ``fauna.bridges.{create,list,update,revoke,delete}_account_alias``
over the real WS-RPC socket with a **User**-class keypair. ``WsRpcAdminClient``
is class-agnostic (its docstring: pass a User-class actor's keypair and you
have a User-class client) — these kinds are User-gated, not Admin: the nest
derives the owning actor from the authenticated caller, so a user manages
their *own* aliases only (``mail-aliases.md`` § Cross-actor isolation).

Independent of the conftest's legacy-HTTP domain seeding: ``create`` does
not validate domain existence (the FK is intentionally loose), so these tests touch no ``mail_domains`` row
and use a synthetic domain. Fresh per-test users keep the per-actor list
assertions deterministic on the session-scoped nest.

A2.2 added wildcard create; A2.3 adds the ``generate_disposable_alias`` mint
(``<handle>-temp-<token>@<domain>``). The fixed-order ``resolve_recipient``
resolver is MTA-class (covered by Rust handler tests, not callable here). A2.4
adds the user-facing ``list_account_alias_hits`` audit reader, exercised below
over the real socket; the resolve→log→list flow that *populates* a hit needs an
MTA-class caller (and the Go-MTA cutover from ``validate_recipient`` is still
pending), so it's covered by the in-process Rust handler test
``resolved_exact_logs_a_hit_listable_by_owner``.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3

DOMAIN = "aliases-e2e.test"


def _fresh_user(nest_instance):
    """A fresh registered (User-class) actor on the shared nest."""
    return create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _controls(label="", spam=None, per_hour=None, per_day=None):
    """Build the AliasControls wire map; omit unset Options (the Rust side
    is `#[serde(default)]`)."""
    c = {"label": label}
    if spam is not None:
        c["spam_threshold_override"] = spam
    if per_hour is not None:
        c["rate_limit_per_hour"] = per_hour
    if per_day is not None:
        c["rate_limit_per_day"] = per_day
    return c


def _create(client, pattern, controls=None, kind="exact"):
    return client.call(
        "fauna.bridges.create_account_alias",
        {
            "kind": kind,
            "local_domain": DOMAIN,
            "pattern": pattern,
            "controls": controls if controls is not None else _controls(),
        },
    )


def _list(client):
    return client.call("fauna.bridges.list_account_aliases", {})["aliases"]


@pytest.mark.feature("mail-aliases")
def test_user_alias_crud_round_trip(nest_instance):
    user = _fresh_user(nest_instance)
    pattern = "bob-" + secrets.token_hex(4)
    with _user_client(nest_instance, user) as client:
        # Fresh user starts with no aliases.
        assert _list(client) == []

        # Create — returns a 16-byte alias_id.
        reply = _create(client, pattern, _controls(label="work", spam=8, per_hour=100))
        alias_id = reply["alias_id"]
        assert isinstance(alias_id, bytes) and len(alias_id) == 16

        # List shows it, owned by the caller, with the controls round-tripped.
        rows = _list(client)
        assert len(rows) == 1
        row = rows[0]
        assert row["pattern"] == pattern
        assert row["kind"] == "exact"
        assert row["actor_id"] == user["actor_id_bytes"]
        assert row["disabled"] is False
        assert row["label"] == "work"
        # Scaled-int (not float) spam threshold survives the REAL column.
        assert row["spam_threshold_override"] == 8
        assert row["rate_limit_per_hour"] == 100
        assert row["rate_limit_per_day"] is None
        assert row["uses_remaining"] is None

        # Update — full-overwrite of pattern + controls (kind immutable).
        new_pattern = "bobby-" + secrets.token_hex(4)
        client.call(
            "fauna.bridges.update_account_alias",
            {
                "alias_id": alias_id,
                "pattern": new_pattern,
                "controls": _controls(label="renamed", spam=3),
            },
        )
        row = _list(client)[0]
        assert row["pattern"] == new_pattern
        assert row["label"] == "renamed"
        assert row["spam_threshold_override"] == 3
        # Overwrite cleared the per-hour cap.
        assert row["rate_limit_per_hour"] is None

        # Revoke — flips disabled, preserves the row.
        client.call("fauna.bridges.revoke_account_alias", {"alias_id": alias_id})
        assert _list(client)[0]["disabled"] is True

        # Delete — row gone.
        client.call("fauna.bridges.delete_account_alias", {"alias_id": alias_id})
        assert _list(client) == []


@pytest.mark.feature("mail-aliases")
def test_create_rejects_reserved_charclass_and_nonexact_kind(nest_instance):
    user = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as e:
            _create(client, "postmaster")
        assert e.value.code == "fauna.bridges.reserved_local_part"

        # mass-mailing #5: `unsubscribe@` / `unsubscribe-*@` are reserved at
        # create time via the separate creation-reserved predicate (NOT the
        # role-routing set — adding it there would mis-route inbound
        # `unsubscribe+<token>@` to the admin mailbox). Exact + the `-`-child
        # family both refuse, independent of the admin reserved list.
        for reserved in ("unsubscribe", "unsubscribe-weekly"):
            with pytest.raises(RpcCallError) as e:
                _create(client, reserved)
            assert e.value.code == "fauna.bridges.reserved_local_part", reserved
        with pytest.raises(RpcCallError) as e:
            _create(client, "unsubscribe-", kind="wildcard_prefix")
        assert e.value.code == "fauna.bridges.reserved_local_part_in_wildcard"

        # '+' is not in the strict ASCII class (sub-addressing is resolver-
        # only, never a stored pattern).
        with pytest.raises(RpcCallError) as e:
            _create(client, "bob+work")
        assert e.value.code == "fauna.protocol.malformed"

        # `disposable` mints via a separate RPC (A2.3) and `catchall` is admin
        # policy — neither is user-creatable here. (exact + wildcard_prefix are;
        # wildcard create has its own tests below.)
        for kind in ("disposable", "catchall"):
            with pytest.raises(RpcCallError) as e:
                _create(client, "bobby", kind=kind)
            assert e.value.code == "fauna.protocol.malformed", f"kind={kind}"

        # None of the rejected attempts wrote a row.
        assert _list(client) == []


@pytest.mark.feature("mail-aliases")
def test_duplicate_pattern_is_conflict_across_users(nest_instance):
    a = _fresh_user(nest_instance)
    b = _fresh_user(nest_instance)
    pattern = "shared-" + secrets.token_hex(4)
    with _user_client(nest_instance, a) as ca:
        _create(ca, pattern)
    # A different user can't claim the same exact alias on the same domain.
    with _user_client(nest_instance, b) as cb:
        with pytest.raises(RpcCallError) as e:
            _create(cb, pattern)
        assert e.value.code == "fauna.bridges.conflicts_with_existing_alias"


@pytest.mark.feature("mail-aliases")
def test_cross_actor_isolation(nest_instance):
    a = _fresh_user(nest_instance)
    b = _fresh_user(nest_instance)
    pattern = "alice-" + secrets.token_hex(4)
    with _user_client(nest_instance, a) as ca:
        alias_id = _create(ca, pattern)["alias_id"]

    with _user_client(nest_instance, b) as cb:
        # B sees none of A's aliases.
        assert _list(cb) == []
        # B can't update / revoke / delete A's alias → not_found (existence
        # is not leaked as permission_denied).
        for kind, payload in [
            (
                "fauna.bridges.update_account_alias",
                {"alias_id": alias_id, "pattern": "hax", "controls": _controls()},
            ),
            ("fauna.bridges.revoke_account_alias", {"alias_id": alias_id}),
            ("fauna.bridges.delete_account_alias", {"alias_id": alias_id}),
        ]:
            with pytest.raises(RpcCallError) as e:
                cb.call(kind, payload)
            assert e.value.code == "fauna.bridges.not_found", f"kind={kind}"

    # A's alias is untouched.
    with _user_client(nest_instance, a) as ca:
        assert any(r["pattern"] == pattern for r in _list(ca))


# ── A2.2 — wildcard-prefix create ────────────────────────────────────
#
# The `resolve_recipient` resolver itself is MTA-class (bridge→nest) and
# covered by Rust handler tests (it mirrors `validate_recipient`, which has
# no e2e and is not yet wired to the Go MTA). The genuinely-new *user*-facing
# surface is wildcard create + its conflict errors — exercised here over the
# real WS socket. Prefixes are randomized so the session-scoped nest's other
# alias rows can't perturb the cross-user/glob assertions.


@pytest.mark.feature("mail-aliases")
def test_wildcard_create_round_trip(nest_instance):
    user = _fresh_user(nest_instance)
    prefix = "wc" + secrets.token_hex(3) + "-"
    with _user_client(nest_instance, user) as client:
        _create(client, prefix, _controls(label="masks"), kind="wildcard_prefix")
        rows = _list(client)
        assert len(rows) == 1
        assert rows[0]["kind"] == "wildcard_prefix"
        assert rows[0]["pattern"] == prefix
        assert rows[0]["label"] == "masks"


@pytest.mark.feature("mail-aliases")
def test_wildcard_create_rejections(nest_instance):
    user = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        # Reserved-glob: `dmarc-*` would shadow `dmarc-report`.
        with pytest.raises(RpcCallError) as e:
            _create(client, "dmarc-", kind="wildcard_prefix")
        assert e.value.code == "fauna.bridges.reserved_local_part_in_wildcard"

        # Structural failures → malformed: too-short lead, no trailing '-',
        # and a literal '*' (the client must strip it).
        for bad in ["b-", "bob", "bob-*"]:
            with pytest.raises(RpcCallError) as e:
                _create(client, bad, kind="wildcard_prefix")
            assert e.value.code == "fauna.protocol.malformed", f"prefix={bad!r}"

        assert _list(client) == []


@pytest.mark.feature("mail-aliases")
def test_wildcard_one_per_actor(nest_instance):
    user = _fresh_user(nest_instance)
    p1 = "wc" + secrets.token_hex(3) + "-"
    p2 = "wd" + secrets.token_hex(3) + "-"
    with _user_client(nest_instance, user) as client:
        _create(client, p1, kind="wildcard_prefix")
        with pytest.raises(RpcCallError) as e:
            _create(client, p2, kind="wildcard_prefix")
        assert e.value.code == "fauna.bridges.actor_already_has_wildcard"


@pytest.mark.feature("mail-aliases")
def test_wildcard_conflicts_with_other_users_exact(nest_instance):
    a = _fresh_user(nest_instance)
    b = _fresh_user(nest_instance)
    prefix = "wc" + secrets.token_hex(3) + "-"
    # A owns an exact alias that the wildcard glob would shadow.
    with _user_client(nest_instance, a) as ca:
        _create(ca, prefix + "foo")
    with _user_client(nest_instance, b) as cb:
        with pytest.raises(RpcCallError) as e:
            _create(cb, prefix, kind="wildcard_prefix")
        assert e.value.code == "fauna.bridges.conflicts_with_existing_alias"


# ── A2.3 disposable mint ──────────────────────────────────────────────


def _mint_disposable(client, ttl_days=None, uses=None, label=""):
    payload = {"label": label}
    if ttl_days is not None:
        payload["ttl_days"] = ttl_days
    if uses is not None:
        payload["uses"] = uses
    return client.call("fauna.bridges.generate_disposable_alias", payload)


@pytest.mark.feature("mail-aliases")
def test_disposable_mint_round_trip(nest_instance):
    user = _fresh_user(nest_instance)
    handle = "bob" + secrets.token_hex(3)
    with _user_client(nest_instance, user) as client:
        # The mint derives <handle> + <domain> from the canonical (oldest)
        # exact alias, so it must exist first.
        _create(client, handle)
        reply = _mint_disposable(client, label="amazon")
        token = reply["token"]
        assert isinstance(reply["alias_id"], bytes) and len(reply["alias_id"]) == 16
        assert len(token) == 6
        assert reply["full_address"] == f"{handle}-temp-{token}@{DOMAIN}"

        # The minted row appears in the list with the disposable controls.
        disp = next(r for r in _list(client) if r["kind"] == "disposable")
        assert disp["pattern"] == token
        assert disp["label"] == "amazon"
        assert disp["uses_remaining"] == 1
        assert disp["expires_at"] is not None
        assert disp["rate_limit_per_day"] == 100


@pytest.mark.feature("mail-aliases")
def test_disposable_mints_capped_per_day(nest_instance):
    """The generator itself is rate-limited: a user mints at most
    `mail.account.disposable_generate_per_day` (default 50) disposables in a
    rolling day, and the next mint is refused with a named code — a compromised
    client cannot enumerate the deployment by minting without end
    (`mail-aliases.md` § Don't do these). The cap is per actor: a second user
    still mints after the first is capped."""
    cap = 50  # libs/fauna-mail/src/aliases/mod.rs::DISPOSABLE_GENERATE_PER_DAY_DEFAULT
    user = _fresh_user(nest_instance)
    other = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        _create(client, "cap" + secrets.token_hex(4))
        for i in range(cap):
            _mint_disposable(client, label=f"m{i}")
        with pytest.raises(RpcCallError) as e:
            _mint_disposable(client, label="one-too-many")
        assert e.value.code == "fauna.bridges.disposable_generate_rate_limited", (
            f"mint {cap + 1} in a day must be refused as rate-limited; got {e.value.code!r}"
        )
        minted = [r for r in _list(client) if r["kind"] == "disposable"]
        assert len(minted) == cap, f"the refused mint must create no row; got {len(minted)}"
    with _user_client(nest_instance, other) as client:
        _create(client, "cap" + secrets.token_hex(4))
        assert _mint_disposable(client)["token"], "another user's cap is untouched"


def test_disposable_requires_canonical_address(nest_instance):
    user = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        # No exact alias → no <handle>/<domain> to mint from.
        with pytest.raises(RpcCallError) as e:
            _mint_disposable(client)
        assert e.value.code == "fauna.bridges.no_canonical_address"


# ── A2.4 list_account_alias_hits (user-facing audit reader) ───────────
#
# The hit *population* path (resolve_recipient → log_alias_hit) is MTA-class
# and covered by Rust handler tests; here we exercise the user-facing reader's
# socket contract: an owned alias with no hits returns an empty page, and the
# owner gate (`not_found`) holds for cross-actor + unknown-alias requests.


def _list_hits(client, alias_id, limit=100, before_hit_id=None):
    payload = {"alias_id": alias_id, "limit": limit}
    if before_hit_id is not None:
        payload["before_hit_id"] = before_hit_id
    return client.call("fauna.bridges.list_account_alias_hits", payload)["hits"]


@pytest.mark.feature("mail-aliases")
def test_list_alias_hits_empty_for_owned_alias(nest_instance):
    user = _fresh_user(nest_instance)
    pattern = "bob-" + secrets.token_hex(4)
    with _user_client(nest_instance, user) as client:
        alias_id = _create(client, pattern)["alias_id"]
        # No hits yet → empty page (the kind is registered + User-gated and the
        # owner happy-path resolves over the real socket). Cursor variant too.
        assert _list_hits(client, alias_id) == []
        assert _list_hits(client, alias_id, limit=10, before_hit_id=alias_id) == []


@pytest.mark.feature("mail-aliases")
def test_list_alias_hits_owner_isolation(nest_instance):
    a = _fresh_user(nest_instance)
    b = _fresh_user(nest_instance)
    pattern = "alice-" + secrets.token_hex(4)
    with _user_client(nest_instance, a) as ca:
        alias_id = _create(ca, pattern)["alias_id"]

    with _user_client(nest_instance, b) as cb:
        # B can't read A's alias hits → not_found (existence not leaked).
        with pytest.raises(RpcCallError) as e:
            _list_hits(cb, alias_id)
        assert e.value.code == "fauna.bridges.not_found"

        # An unknown alias is also not_found.
        with pytest.raises(RpcCallError) as e:
            _list_hits(cb, secrets.token_bytes(16))
        assert e.value.code == "fauna.bridges.not_found"
