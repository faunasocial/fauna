"""Mail-alias helpers: the harness's one alias write (:func:`add_exact_alias`),
and which domain the mail-aliases page will mint on — asked, never assumed.

Every localpart-only gesture on the mail-aliases page (the add-sheet's Create,
the disposable mint, an import line typed without a domain) lands on the client
snapshot's ``default_domain``, and that has exactly one writer:
``MailAliasesMachine::refresh`` in
``libs/fauna-client-mail-settings/src/aliases.rs``, deriving it from the listed
rows (``docs/goal/behavior/mail-aliases.md:444``).

⚠ **The derivation prefers the CANONICAL row**, and a test's own seeded exact
alias is not it. ``derive_default_domain`` (``aliases.rs:491``) reads
canonical → first exact → first, and every actor already owns a canonical
``<handle>@<domain>`` alias that mail-enable writes and that
``delete_account_alias`` refuses to remove (``mail-aliases.md:166``). So an e2e
that seeds an exact alias on its own domain and then expects the add-sheet to
mint there is asserting against a domain the product will never choose.

That is exactly how three linux e2e tests broke: they hard-coded their seeded
domain, a later change (2026-08-28) moved the derivation from "oldest
exact alias" — which had never actually found the oldest one — to
canonical-first, and from that sweep on the tests read
``shop…@fauna.test`` where they wanted ``shop…@aliases-ux-e2e.test``. Asking the nest which row is canonical keeps
the seeded domain useful for what it is actually good for — proving the
derivation ignores it.
"""

from __future__ import annotations

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

_ALIAS_CONFLICT = "fauna.bridges.conflicts_with_existing_alias"
_ALIAS_CAP = "fauna.bridges.alias_cap_exceeded"


def add_exact_alias(nest_url: str, signing_key, domain: str, local_part: str) -> str:
    """Give the member holding ``signing_key`` the address ``<local_part>@<domain>``
    and return it — the harness's one way to make a test address routable.

    It is the member's own write (``fauna.bridges.create_account_alias``, signed
    with the member's key), because that is the only alias write the nest has: an
    admin neither sees nor edits another member's aliases (``mail-aliases.md``
    § Cross-actor isolation). So the address is subject to everything the product
    enforces on a member — a reserved local-part and the per-account cap are
    refused here exactly as they are in the app. An admin who wants an address of
    their own passes their own key (Admin ⊇ User).

    Safe to repeat: see :func:`add_exact_alias_as`, which does the write and which
    a caller already holding a client open as the member calls directly.
    """
    with WsRpcAdminClient(
        nest_url,
        actor_id=bytes(signing_key.verify_key),
        signing_key=bytes(signing_key),
    ) as member:
        return add_exact_alias_as(member, domain, local_part)


def add_exact_alias_as(member: WsRpcAdminClient, domain: str, local_part: str) -> str:
    """:func:`add_exact_alias` over ``member``, an open client authenticated as the
    member who is to own the address.

    Safe to repeat: a second call for an address the member already holds — a
    session-scoped member reused across modules, or the canonical
    ``<handle>@<domain>`` the nest wrote at mail-enable — returns without
    writing. An address *another* member holds stays the refusal it is.
    """
    try:
        member.call(
            "fauna.bridges.create_account_alias",
            {
                "kind": "exact",
                "local_domain": domain,
                "pattern": local_part,
                "controls": {"label": ""},
            },
        )
    except RpcCallError as err:
        if err.code == _ALIAS_CAP:
            raise AssertionError(
                f"this member already holds the most exact aliases one account may "
                f"({err!r}), so {local_part}@{domain} was refused — a session-shared "
                f"member collects one per mail test; reuse a fixed local-part, or give "
                f"the test a member of its own"
            ) from err
        if err.code != _ALIAS_CONFLICT:
            raise
        rows = member.call("fauna.bridges.list_account_aliases", {})["aliases"]
        held = any(
            row["kind"] == "exact"
            and row["local_domain"].lower() == domain.lower()
            and row["pattern"].lower() == local_part.lower()
            and not row["disabled"]
            for row in rows
        )
        if not held:
            raise AssertionError(
                f"{local_part}@{domain} is already held by another member (or by a "
                f"forwarder or list), so this member cannot take it — give the test "
                f"a local-part of its own. The member's own rows: {rows!r}"
            ) from err
    return f"{local_part}@{domain}"


def default_alias_domain(api) -> str:
    """The domain the page's localpart-only gestures will mint on.

    ``api`` is an open user-class ``WsRpcAdminClient``. Mirrors
    ``derive_default_domain``'s canonical → first-exact → first order over the
    same rows the client lists, so a test asserts against the product's own
    answer rather than a constant that drifts out from under it.
    """
    rows = api.call("fauna.bridges.list_account_aliases", {})["aliases"]
    if not rows:
        raise AssertionError(
            "the actor has no aliases at all, so the mail-aliases page has no "
            "`default_domain` and every mutating control on it is disabled "
            "(mail-aliases.md:444) — seed one before asking"
        )
    # ⚠ Loud, not quiet, if the flag ever leaves the wire. Reading it with a
    # plain `.get` would fall through to the first-exact arm and hand back the
    # caller's own seeded domain — which is precisely the pre-fix
    # answer, so the test would fail exactly as it did before with no hint that
    # the helper had stopped working. `mail-aliases.md:387` ratifies the field.
    if not any("is_canonical" in row for row in rows):
        raise AssertionError(
            "no listed alias row carries `is_canonical`, but "
            "`fauna.bridges.list_account_aliases` is specified to flag the "
            "canonical row (mail-aliases.md:387) — the wire drifted, and this "
            f"helper cannot mirror `derive_default_domain` without it: {rows!r}"
        )
    for row in rows:
        if row.get("is_canonical"):
            return row["local_domain"]
    for row in rows:
        if row.get("kind") == "exact":
            return row["local_domain"]
    return rows[0]["local_domain"]
