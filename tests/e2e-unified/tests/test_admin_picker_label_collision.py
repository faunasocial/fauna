"""tier_3 e2e: admin picker option TEXT stays injective when two users share a
display LABEL but hold distinct HANDLES (``admin.md`` § 2 → *What identifies a
user in an admin picker* — the two-halves rule, ratified 2026-08-30).

Regression pin for a gap this test closes: web's own wasm-level pin
(``apps/fauna-web/src/lib/admin-picker-option-injectivity.test.ts``) cannot
import the three real ``.svelte`` build sites at all (``$app/paths`` only
resolves under the SvelteKit/Vite build), and tui's DNS/apex collision tests
(``apps/fauna-tui/src/admin/mod.rs``) build their fixture by calling
``fauna_client_admin::actor_picker_options`` directly instead of driving
``Op::LoadDns`` / ``load_web_snapshot`` — the real call sites. Neither app had
a test that would catch a regression to the freely-editable, non-unique
``label`` AT the real call site, and no existing e2e test ever seeds two users
sharing a label — see ``test_family.py``'s own note that every caller there
already passes its own distinct handle *as* the label, so this exact
collision cannot arise in that module.

Drives the three families ``admin.md`` names as sharing the rule — the
admin-users guardian picker (Invite section), the admin-dns catch-all +
role-address pickers, and the admin-web apex picker — and asserts the FULL
painted option-text list (``option_texts``, not a single selected-value
read-back) is pairwise distinct. A read-back of the currently-selected option
can't tell two same-labelled users apart once one is picked — on web the
option's DOM ``value`` already equals its visible text, so selecting by that
text and reading the same text back proves nothing about the rest of the
list — whereas listing every painted option catches a same-text collision
directly.

Wires ``PlatformDriver.option_texts`` (new — reads a synthesized ``"options"``
``get_attr``: a JSON array of everything a picker currently painted) for
**web** and **tui** only, the two apps with an open regression gap;
linux/android/apple/windows already carry their own real-build-site pin for
at least part of this property (``admin.md`` § 2's landed history) and can
adopt ``option_texts`` if/when their own gap is filed — mirrors
``test_admin_role_address.py``'s own android note.
"""

import secrets

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import register_user
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.web, pytest.mark.tui]


def _admin_client(nest_instance) -> WsRpcAdminClient:
    """Mirrors ``test_admin_catch_all.py``/``test_admin_role_address.py``'s own
    helper of the same name — one canonical Admin-class WS-RPC connection."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _register(nest_instance, handle: str, label: str) -> str:
    """Admit a plain non-admin, non-suspended user under `handle`/`label` over
    the admin WS-RPC control plane (`fauna.admin.users.create` via
    `common.auth.register_user`) — fixture SETUP, never the action under test
    (convention 8(b), `e2e-conventions.md:92`). Returns the actor id hex."""
    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    register_user(
        nest_instance["port"],
        actor_hex,
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
        handle=handle,
        label=label,
    )
    return actor_hex


def _relabel(nest_instance, actor_hex: str, label: str) -> None:
    """`fauna.admin.users.update` — an admin free-editing a user's LABEL after
    creation (`admin.md` § 2: "labels are freely editable"), the exact act
    that can collide two accounts' display text. Fixture setup: arranging the
    collision precondition, never a picker mutation under test."""
    with _admin_client(nest_instance) as client:
        client.call(
            "fauna.admin.users.update",
            {"actor_id": bytes.fromhex(actor_hex), "tier": "free", "label": label},
        )


@pytest.fixture(scope="module")
def colliding_users(nest_instance) -> tuple[str, str]:
    """Two non-suspended, non-admin users holding DISTINCT handles, both
    relabelled to the SAME display label — the collision the two-halves rule
    exists to survive.

    Module-scoped: unlike `test_admin_role_address.py`/`test_admin_catch_all.py`
    (which re-seed a fresh DOMAIN per test because they *mutate* a per-domain
    override another app run could leave dirty), these two users are read-only
    fixtures for every test in this module — nothing here selects or persists
    a designation involving them — so one seed serves every test and app
    parametrization sharing the session-scoped `nest_instance`.
    """
    handle_a = f"collidea{secrets.token_hex(3)}"
    handle_b = f"collideb{secrets.token_hex(3)}"
    actor_a = _register(nest_instance, handle_a, f"orig-a-{secrets.token_hex(2)}")
    actor_b = _register(nest_instance, handle_b, f"orig-b-{secrets.token_hex(2)}")
    label = f"same-label-{secrets.token_hex(3)}"
    _relabel(nest_instance, actor_a, label)
    _relabel(nest_instance, actor_b, label)
    return handle_a, handle_b


@pytest.fixture(scope="module")
def collision_domain(nest_instance) -> str:
    """A local mail domain seeded once for the admin-dns pickers below — safe
    to share across tests/apps here because nothing in this module selects or
    clears a designation on it (mirrors `colliding_users`' own reasoning)."""
    domain = f"pickercollide-{secrets.token_hex(4)}.test"
    with _admin_client(nest_instance) as client:
        client.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": domain,
                "mta_sts_cert_mode": "expand_primary",
            },
        )
    return domain


def _wait_for_both_handles(
    driver, element_id: str, handle_a: str, handle_b: str, *, index: int = 0
) -> list[str]:
    """Poll `option_texts(element_id)` until it paints BOTH handles (the
    async users-list fetch every one of these pickers rides can leave the
    picker at just its "None" sentinel for a beat after navigation — a plain
    truthy/non-empty check would return on that incomplete list). Returns the
    full option list once ready, for the injectivity assertion."""

    def _ready():
        opts = driver.option_texts(element_id, index=index)
        if opts and handle_a in opts and handle_b in opts:
            return opts
        return None

    return wait_until(
        _ready,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{element_id!r} never painted both {handle_a!r} and {handle_b!r}; "
            f"last seen options={driver.option_texts(element_id, index=index)!r}; "
            f"{driver.diagnose(element_id)}"
        ),
    )


def _assert_injective(options: list[str], handle_a: str, handle_b: str, where: str) -> None:
    assert options.count(handle_a) == 1, (
        f"{where}: expected {handle_a!r} exactly once, got {options!r}"
    )
    assert options.count(handle_b) == 1, (
        f"{where}: expected {handle_b!r} exactly once, got {options!r}"
    )
    assert len(options) == len(set(options)), (
        f"{where}: two options rendered identically — collided on the "
        f"(freely-editable, non-unique) label instead of the handle: {options!r}"
    )


@pytest.mark.feature("admin-users")
def test_admin_users_guardian_picker_stays_injective_when_labels_collide(
    admin_app, colliding_users
):
    """`admin-users-invite-guardian-select` (Invite section) renders both
    colliding users' HANDLES distinctly — pins the real
    `admin/users/+page.svelte:122` build site on web, and tui's
    `guardian_picker` build site (`apps/fauna-tui/src/admin/users.rs`)."""
    handle_a, handle_b = colliding_users
    admin_app.admin.navigate_users()
    admin_app.driver.click("create-invite-code-btn")
    admin_app.driver.wait_for("admin-users-invite-guardian-select", timeout=10.0)
    options = _wait_for_both_handles(
        admin_app.driver, "admin-users-invite-guardian-select", handle_a, handle_b
    )
    _assert_injective(options, handle_a, handle_b, "admin-users-invite-guardian-select")


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_pickers_stay_injective_when_labels_collide(
    admin_app, colliding_users, collision_domain
):
    """`admin-dns-domain-catch-all-select` and one
    `admin-dns-domain-role-address-<role>-select` render both colliding
    users' HANDLES distinctly — pins the real `admin/dns/+page.svelte:323`
    build site on web, and tui's `Op::LoadDns` build site
    (`apps/fauna-tui/src/admin/mod.rs`) — the two share one `actors` list on
    each app, so one role suffices to exercise the call site both pickers
    read from."""
    handle_a, handle_b = colliding_users
    admin_app.admin.navigate_dns()
    wait_until(
        lambda: admin_app.admin.dns_domain_names()
        if collision_domain in admin_app.admin.dns_domain_names()
        else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {collision_domain!r} not rendered on admin-dns; "
            f"got {admin_app.admin.dns_domain_names()!r}"
        ),
    )
    idx = admin_app.admin.dns_domain_names().index(collision_domain)

    catch_all_options = _wait_for_both_handles(
        admin_app.driver, "admin-dns-domain-catch-all-select", handle_a, handle_b, index=idx
    )
    _assert_injective(catch_all_options, handle_a, handle_b, "admin-dns-domain-catch-all-select")

    role_options = _wait_for_both_handles(
        admin_app.driver,
        "admin-dns-domain-role-address-abuse-select",
        handle_a,
        handle_b,
        index=idx,
    )
    _assert_injective(
        role_options, handle_a, handle_b, "admin-dns-domain-role-address-abuse-select"
    )


@pytest.mark.feature("admin-front-page")
def test_admin_web_apex_picker_stays_injective_when_labels_collide(admin_app, colliding_users):
    """`admin-web-apex-actor-select` renders both colliding users' HANDLES
    distinctly — pins the real `admin/web/+page.svelte:85` build site on web,
    and tui's `load_web_snapshot` build site
    (`apps/fauna-tui/src/admin/mod.rs`)."""
    handle_a, handle_b = colliding_users
    admin_app.admin.navigate_web()
    options = _wait_for_both_handles(
        admin_app.driver, "admin-web-apex-actor-select", handle_a, handle_b
    )
    _assert_injective(options, handle_a, handle_b, "admin-web-apex-actor-select")
