"""Cross-app community-labeler catalog — browse + inspect-before-subscribe +
(un)subscribe, plus the Personalization home's subscribed-labelers facet
(``docs/goal/architecture/content-moderation-and-ranking.md`` § Tier-3
community models; ``tests/e2e-unified/ui.yaml`` ``personalization`` +
``labeler-catalog`` pages). The nest wire
(``fauna.labelers.{list,inspect,subscribe,unsubscribe}``) is already
tier_3-proven server-side by ``test_capability_labeler_drain.py``; this test
proves the client UI surface built on top of it.

tier_3: every binary is real (nest + client driver), the wire is real, no
mocks. A signed ``content_kind="post"`` (PUBLIC) labeler is PUBLISHED as a
fixture precondition via the seal-helper ``publish-labeler`` mode +
``fauna.labelers.publish`` — publishing is a developer/publisher action, not a
client-UI-drivable *user* gesture in v1, so this is fixture setup arranging
the precondition, not the mutation under
test. A public-kind labeler needs no capability grant to subscribe (only a
restricted kind, e.g. mail, would — that leg is v1-inert, ``grant_id: None``
always; tracked internally). The mutations under
test — browse, inspect, subscribe, unsubscribe — are driven through the real
client UI (a real UI-driven mutation, not an API shortcut).

The catalog is nest-global (``fauna.labelers.list`` has no per-actor filter),
not per-actor, so this test locates its own published row by its unique
``factor`` string rather than assuming a position or an absolute count — the
shared session nest may carry labelers from other runs/tests. The
Personalization home's SUBSCRIBED facet, by contrast, genuinely is per-actor
(``LabelerSummary.subscribed`` is stamped from the caller's own
``labeler_subscriptions`` rows), so a fresh ``logged_in_app`` session safely
starts at zero subscriptions.

linux + web + windows ship the surface as of 2026-07-08; apple (macOS + iOS)
ships as of 2026-07-12; android ships as of 2026-07-17 and is compile- and
Robolectric-verified (the old "pending gradle compile-verify, Rosetta disabled
on the Linux dev VM" note here was stale: Rosetta was unblocked 2026-07-16 via
``CAMBRIA_DISABLE_AOT=1``, and the android leg landed the next day).

``@pytest.mark.android`` was withheld here for a while — no android e2e test
has ever run against a real device fleet-wide (the android bridge implements
no ``/element/attr`` and no ``/element/enabled`` route, and emulator access is
emulator-host-gated; that gap is tracked) — but the mark
on ``test_labeler_catalog_browse_inspect_subscribe_unsubscribe`` below uses
neither route (only visibility/text/count reads over
``LabelerCatalogActions``), and the feature catalog's parity bookkeeping
(``docs/goal/architecture/feature-catalog.md`` § Cell semantics) treats the
coverage-contract mark and an actual device run as two different questions:
the mark records that android is built-and-Robolectric-proven (which it is,
per this docstring's own 2026-07-17 note above), while the MATRIX.md cell
stays blank until a real device run lands. Marked accordingly (2026-09-11) —
a mark that cannot run yet still proves the client surface exists and is
tested one tier down; add per-test marks to the file's other tests as their
own bridge dependencies (or lack thereof) are checked the same way.
"""

import base64
import sqlite3
import time
from pathlib import Path

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.labeler_publish import publish_wasm_labeler
from helpers.nest_trust_setup import login_as_admin, seed_mda_holder
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# The committed BARE-emitting fixture WASM, reused from
# test_capability_labeler_drain.py — its content doesn't matter here (no
# scoring is exercised), only that it's a valid signed module to publish.
_CAT_WAT_PATH = (
    Path(__file__).resolve().parent.parent / "fixtures" / "labeler" / "cat_labeler.wat"
)


def _publish_post_labeler(run_seal_helper, nest_instance) -> tuple[bytes, str]:
    """Publish a signed, PUBLIC (``content_kind="post"``) community labeler as
    a fresh registered User whose keypair IS the labeler's ``algorithm_id``
    (signer; public-key-is-identity) — mirrors
    ``test_capability_labeler_drain._publish_cat_labeler``, but
    ``content_kind="post"`` so subscribing needs no capability grant (the
    restricted-kind mint leg is v1-inert and out of scope here). Returns
    ``(labeler_id_bytes, factor_str)``."""
    labeler_id = publish_wasm_labeler(run_seal_helper, nest_instance, _CAT_WAT_PATH)
    return labeler_id, "labeler:" + labeler_id.hex()


# This build's text-model tokenizer contract
# (``fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION``). Deliberately a literal:
# the point of the pair below is that ONE of the two artifacts is a version this
# build implements and the other is not, and a mirror that silently followed a
# Rust bump would make the "supported" half stop proving anything. When the
# constant bumps, this fails loudly on the ordinary row — update it here.
_SUPPORTED_ARTIFACT_VERSION = 1

# A tokenizer contract no build implements (and none soon will). Any version
# this build does not implement does, so the exact number is not load-bearing —
# only that ``text_model_version_supported`` reads it as false.
_UNSUPPORTED_ARTIFACT_VERSION = 4242


def _publish_text_model_labeler(
    run_seal_helper, nest_instance, *, version: int, name: str
) -> str:
    """Publish a PUBLIC ``text-model`` labeler whose artifact claims ``version``,
    as a fresh registered User whose keypair IS the ``algorithm_id``. Returns the
    row's ``factor`` string.

    The artifact is minted by the seal-helper's ``encode-text-model-artifact``
    mode because **no app UI can publish one at a chosen version** — the shared
    publish lifecycle stamps ``TEXT_MODEL_ARTIFACT_VERSION`` itself, deliberately
    (it is the subscriber's tokenizer contract, never a publisher's claim). That
    is precisely why the badge under test cannot be reached through a UI gesture,
    and convention 8 carve-out (b) covers it: this is fixture setup arranging the
    precondition, and the mutation under test is the catalog *render*."""
    admin = nest_instance["admin"]
    publisher = create_actor_and_register(
        nest_instance["port"], base_url=nest_instance["url"],
        admin_signing_key=admin["signing_key"],
    )
    pub_seed = bytes(publisher["signing_key"])
    labeler_id = publisher["actor_id_bytes"]  # == algorithm_id (verify key)

    artifact = run_seal_helper(
        "encode-text-model-artifact",
        {
            "version": version,
            "name": name,
            # The counts are the corpus the vocabulary was built from; they must
            # clear TEXT_MODEL_PUBLISH_MIN_DOCS or the nest's gate refuses the
            # artifact whoever built it (the structural privacy floor).
            "more_docs": 3,
            "less_docs": 0,
            "ngrams": [{"ngram": "kittens", "more": 3, "less": 0}],
        },
    )
    metadata_blob = run_seal_helper(
        "publish-labeler",
        {
            "signing_seed_b64": base64.b64encode(pub_seed).decode(),
            # `wasm_bytes` carries the artifact for every non-wasm kind — the
            # field name is kept for in-major wire compat; the hash/size binding
            # and the signature verify are identical for all three kinds.
            "wasm_b64": base64.b64encode(artifact).decode(),
            "version": 1,
            "needs_text": True,
            "needs_hashtags": False,
            "needs_media_metadata": False,
            "needs_author": False,
            "max_memory_bytes": 16 * 1024 * 1024,
            "max_cpu_microseconds": 100_000,
            "updated_at": int(time.time()),
        },
    )
    pub_ws = WsRpcAdminClient(nest_instance["url"], actor_id=labeler_id, signing_key=pub_seed)
    with pub_ws:
        reply = pub_ws.call(
            "fauna.labelers.publish",
            {
                "metadata_blob": metadata_blob,
                "wasm_bytes": artifact,
                "content_kind": "post",
                "artifact_kind": "text-model",
            },
        )
    assert reply.get("ok") is True, (
        f"text-model publish reply not ok: {reply!r} — the nest gate accepts an "
        f"unrecognized artifact version on purpose (the version is the "
        f"subscriber's contract, not the nest's), so a refusal here is a real "
        f"regression, not this fixture reaching too far"
    )
    return "labeler:" + bytes(reply["labeler_id"]).hex()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("community-labelers")
def test_an_unsupported_artifact_version_says_so_on_its_catalog_row(
    logged_in_app, nest_instance, run_seal_helper
):
    """A ``text-model`` labeler whose artifact claims a tokenizer contract THIS
    build does not implement renders "needs a newer app" on its catalog row,
    and an ordinary one still renders its plain kind.

    This is the *says so* half of the unknown-version contract
    (``content-moderation-and-ranking.md`` § Tier-3 artifact kinds: "a client
    meeting an unknown artifact ``version`` treats the factor as **inert and
    says so**, never silently mis-scoring"). The *inert* half — the scorer
    declining to run it — is tier_1-covered at the one predicate both halves
    read (``fauna_core::scoring::text_model_version_supported``); what only a
    real stack can prove is that the number survives the wire end to end: the
    nest decodes it at its publish gate, stores it in ``labelers.artifact_version``,
    projects it through the metadata-only ``fauna.labelers.list`` browse, and the
    app renders the badge off it — no artifact fetch anywhere on that path.

    Both rows are asserted in ONE journey on purpose: a badge that lit for every
    ``text-model`` row would pass a one-row test while making the catalog useless,
    and the ordinary row is what pins that the substitution is version-driven."""
    app = logged_in_app

    unsupported_factor = _publish_text_model_labeler(
        run_seal_helper, nest_instance,
        version=_UNSUPPORTED_ARTIFACT_VERSION, name="a model from the future",
    )
    ordinary_factor = _publish_text_model_labeler(
        run_seal_helper, nest_instance,
        version=_SUPPORTED_ARTIFACT_VERSION, name="a model this build reads",
    )

    app.labeler_catalog.navigate_catalog()

    ordinary_index = app.labeler_catalog.find_index_by_factor(ordinary_factor)
    assert ordinary_index is not None, (
        f"the ordinary text-model labeler never appeared in the catalog; "
        f"error={app.error_text()!r}"
    )
    assert app.labeler_catalog.item_kind(ordinary_index) == "text-model", (
        f"a supported artifact must still paint the raw discriminator — ui.yaml "
        f"pins this element to a stable list/wasm/text-model value and two "
        f"landed journeys assert exact equality on it; got "
        f"{app.labeler_catalog.item_kind(ordinary_index)!r}"
    )

    unsupported_index = app.labeler_catalog.find_index_by_factor(unsupported_factor)
    assert unsupported_index is not None, (
        f"the future-version labeler never appeared in the catalog — the nest "
        f"must publish and list it like any other, since refusing an unknown "
        f"version would make an older nest reject a newer client's artifact; "
        f"error={app.error_text()!r}"
    )
    assert (
        app.labeler_catalog.item_kind(unsupported_index)
        == S.labeler_catalog.kind_needs_newer_app
    ), (
        f"an artifact version this build does not implement must SAY so on the "
        f"row, not silently offer a factor that would never score; got "
        f"{app.labeler_catalog.item_kind(unsupported_index)!r}, expected "
        f"{S.labeler_catalog.kind_needs_newer_app!r}"
    )


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("community-labelers")
def test_labeler_catalog_browse_inspect_subscribe_unsubscribe(
    logged_in_app, nest_instance, run_seal_helper
):
    """End-to-end: a published public labeler appears in the catalog with its
    signed metadata, the inspect-before-subscribe panel renders it, subscribing
    flips the row's toggle and surfaces the labeler on the Personalization
    home, and unsubscribing reverses both — proving the full
    browse/inspect/subscribe/unsubscribe round-trip over the real wire."""
    app = logged_in_app
    cat = app.labeler_catalog

    labeler_id, factor = _publish_post_labeler(run_seal_helper, nest_instance)

    # Personalization home: this actor has no subscriptions yet. Per-actor
    # state (LabelerSummary.subscribed is stamped from the caller's own
    # labeler_subscriptions rows), so this is safe to assert absolutely even
    # though the catalog itself is nest-global.
    cat.navigate_home()
    # Wait for the read to RESOLVE before believing a zero count — an unloaded
    # page also counts zero rows, so the assertion below is vacuous without this
    # (`ui/README.md` § List pages: loading is not empty).
    cat.wait_for_loaded("personalization-labelers-empty")
    assert cat.wait_for_subscribed_count(0) == 0, (
        f"expected no subscribed labelers before subscribing; error={app.error_text()!r}"
    )
    assert cat.is_subscribed_empty_visible(), "empty-state placeholder missing before subscribing"
    assert not app.has_error(), f"unexpected error on personalization home: {app.error_text()!r}"

    # labeler-catalog page: the published labeler is listed with its signed
    # metadata. Locate it by factor (not position/count — the catalog is
    # nest-global and may carry entries from elsewhere).
    cat.navigate_catalog()
    index = cat.find_index_by_factor(factor)
    assert index is not None, (
        f"published labeler {factor!r} not found among the catalog's "
        f"{cat.catalog_count()} row(s); error={app.error_text()!r}"
    )
    assert not cat.is_catalog_empty_visible(), "empty state should not show once a labeler is published"
    assert cat.item_content_kind(index) == "post", (
        f"expected content_kind 'post', got {cat.item_content_kind(index)!r}"
    )
    assert cat.item_version(index) == "1", f"expected version '1', got {cat.item_version(index)!r}"
    assert cat.item_publisher(index), "publisher field should render non-empty"

    # Not yet subscribed: subscribe visible, unsubscribe hidden (exactly one
    # of the pair renders per row, gated on entry.subscribed).
    assert cat.is_subscribe_visible(index), "unsubscribed row should show the subscribe button"
    assert not cat.is_unsubscribe_visible(index), "unsubscribed row should not show the unsubscribe button"

    # Inspect-before-subscribe: the full signed metadata renders before the
    # user grants this labeler their content.
    cat.inspect(index)
    assert cat.wait_for_inspect_panel(True), "inspect panel did not open"
    metadata = cat.inspect_metadata_text()
    assert metadata, "inspect panel should render non-empty metadata"
    assert "1" in metadata, f"inspect metadata should include the version (1): {metadata!r}"
    # The fifth v1-flag line (content-moderation-and-ranking.md § Tier-3 →
    # *The attachment facet*), on every app now.
    # `false` only: the seal-helper test fixture never sets this flag, so a
    # published test labeler never declares it.
    assert "needs_attachment_bytes: false" in metadata, (
        f"inspect metadata should include the fifth v1 flag: {metadata!r}"
    )
    cat.close_inspect()
    assert cat.wait_for_inspect_panel(False), "inspect panel did not close"

    # Subscribe: the round-trip (fauna.labelers.subscribe -> refresh) flips
    # this row's toggle.
    cat.subscribe(index)
    assert cat.wait_for_subscribed_state(index, subscribed=True), (
        f"subscribe did not flip the row to subscribed; error={app.error_text()!r}"
    )
    assert not app.has_error(), f"unexpected error after subscribe: {app.error_text()!r}"

    # Personalization home now shows exactly the one subscribed labeler, with
    # only the unsubscribe affordance (no inspect/subscribe on this facet).
    cat.navigate_home()
    assert cat.wait_for_subscribed_count(1) == 1, (
        f"expected the subscribed labeler on the personalization home; error={app.error_text()!r}"
    )
    assert not cat.is_subscribed_empty_visible(), "empty state should not show once subscribed"
    assert cat.item_factor(0) == factor, f"expected factor {factor!r}, got {cat.item_factor(0)!r}"
    assert not cat.is_subscribe_visible(0), "personalization home should never show a subscribe button"
    assert cat.is_unsubscribe_visible(0), "the subscribed row should show unsubscribe on the home"

    # Unsubscribe from the home reverses both facets.
    cat.unsubscribe(0)
    assert cat.wait_for_subscribed_count(0) == 0, (
        f"unsubscribe did not clear the personalization home; error={app.error_text()!r}"
    )
    assert cat.is_subscribed_empty_visible(), "empty state should return once unsubscribed"
    assert not app.has_error(), f"unexpected error after unsubscribe: {app.error_text()!r}"

    cat.navigate_catalog()
    assert cat.wait_for_subscribed_state(index, subscribed=False), (
        f"unsubscribe did not flip the catalog row back; error={app.error_text()!r}"
    )


@pytest.fixture
def labeler_grant_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest — own admin, own grant table, own holder roster
    — so the grant rows read below are this test's own (the
    `trust_grant_nest` shape of `test_nest_trust_grants.py`)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "labeler-grant-nest")
    yield nest
    cleanup()


def _owner_grant_rows(nest) -> list[tuple[bytes, bytes, bytes]]:
    """`(grant_id, holder_pubkey, blob)` for every grant the nest holds for its
    admin — the rows a holder's `fauna.capabilities.fetch` answers from."""
    owner = bytes.fromhex(nest["admin"]["actor_id_hex"])
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        return conn.execute(
            "SELECT grant_id, holder_pubkey, blob FROM capability_grants"
            " WHERE owner_actor_id = ? ORDER BY created_at",
            (owner,),
        ).fetchall()
    finally:
        conn.close()


_NO_ROW = object()


def _subscription_grant_id(nest, labeler_id: bytes):
    """The `labeler_subscriptions.grant_id` the nest links the admin's
    subscription to `labeler_id` with: the 16-byte id, `None` for a
    subscription registered without a grant, or `_NO_ROW` when the
    subscription row itself is gone."""
    owner = bytes.fromhex(nest["admin"]["actor_id_hex"])
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        row = conn.execute(
            "SELECT grant_id FROM labeler_subscriptions"
            " WHERE owner_actor = ? AND labeler_id = ?",
            (owner, labeler_id),
        ).fetchone()
    finally:
        conn.close()
    return _NO_ROW if row is None else row[0]


def _wait_on_nest(read, predicate, what: str, budget_s: float = 20.0):
    """Poll `read()` off the nest's own tables until `predicate` holds — a
    deadline poll, never a settle-sleep (convention 14)."""
    deadline = time.monotonic() + budget_s
    value = read()
    while time.monotonic() < deadline:
        value = read()
        if predicate(value):
            return value
        time.sleep(0.5)
    raise AssertionError(f"never observed on the nest: {what}; now {value!r}")


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.feature("community-labelers")
def test_subscribing_a_mail_labeler_trusts_the_mail_service_with_it_and_unsubscribing_withdraws_it(
    app, labeler_grant_nest, run_seal_helper
):
    """Subscribing a ``wasm`` labeler over sealed mail IS minting it a
    capability (`content-moderation-and-ranking.md` § Tier-3 → *Subscribing =
    minting a capability*): the app mints the PER-LABELER grant to the nest's
    mail service — every tuple and every wrap confined to ``labeler:<hex>`` —
    records it in the owner's signed grant log before depositing it, and the
    nest links the subscription row to it 1:1; the Nests page lists the trust
    naming that one labeler (`nests.md` § Trust facet — grants → *the
    per-labeler grant*); unsubscribing revokes it (the nest row is gone, and
    History holds the mint beside the revoke).

    The mutation under test is the app's subscribe/unsubscribe over the real
    wire; the published mail labeler, the enabled mail and the enrolled ``mda``
    holder are fixture preconditions (convention 8 carve-out (b)). Read off
    the nest's own tables: the ``capability_grants`` row IS what the holder's
    next fetch answers from, and ``labeler_subscriptions.grant_id`` IS the
    1:1 link — no holder process is needed to witness either. The grant's
    scope is decoded from the deposited blob itself (canonical dag-cbor), so
    the factor confinement is asserted on the bytes the holder would open,
    not on a client-side claim.

    Every app builds the grant-wired catalog machine (§ Implementation status
    today, leg 5); android skips at the action layer (its e2e is
    emulator-gated fleet-wide). macos is journey-proven. windows and ios are
    wired but not yet journey-proven on their own machines, so their marks
    land with that verification rather than promising a witness nobody has
    run.
    """
    import cbor2

    app.nest_trust.require_mint_test_setup_supported()
    nest = labeler_grant_nest
    login_as_admin(app, nest, device_id="test-device-labeler-grant")

    # Preconditions: mail enabled for the owner (the MSEK the grant's payloads
    # derive from), an `mda` holder to seal to, a published mail labeler.
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    holder_x25519 = seed_mda_holder(nest, bridge_id="labeler-grant-mda")
    labeler_id = publish_wasm_labeler(
        run_seal_helper, nest, _CAT_WAT_PATH, content_kind="mail"
    )
    factor = "labeler:" + labeler_id.hex()
    assert _owner_grant_rows(nest) == [], "a fresh nest holds no grant for its admin yet"

    # ── Subscribe from the catalog: the per-labeler grant is minted, recorded,
    # deposited and linked, all as one gesture.
    cat = app.labeler_catalog
    cat.navigate_catalog()
    index = cat.find_index_by_factor(factor)
    assert index is not None, (
        f"published mail labeler {factor!r} not found among the catalog's "
        f"{cat.catalog_count()} row(s); error={app.error_text()!r}"
    )
    assert cat.item_content_kind(index) == "mail"
    cat.subscribe(index)
    assert cat.wait_for_subscribed_state(index, subscribed=True), (
        f"subscribe did not flip the row to subscribed; error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        "with mail enabled and a holder enrolled the mint owes no notice; "
        f"got {app.error_text()!r}"
    )

    rows = _wait_on_nest(
        lambda: _owner_grant_rows(nest), lambda r: len(r) == 1,
        "the per-labeler grant deposited",
    )
    grant_id, holder_pubkey, blob = rows[0]
    assert holder_pubkey == holder_x25519, "sealed to the mail service's seal target"
    decoded = cbor2.loads(blob)
    assert [t.get("factor") for t in decoded["scope"]] == [factor, factor], (
        f"every declared tuple must be confined to the labeler: {decoded['scope']!r}"
    )
    assert all(
        w["scope"].get("factor") == factor and w.get("epoch") is not None
        for w in decoded["wrapped_keys"]
    ), "every wrap is per-epoch AND confined to the labeler — the license rides the AAD"
    assert _subscription_grant_id(nest, labeler_id) == grant_id, (
        "the subscription row links to the grant it was minted with (1:1)"
    )

    # ── The Nests page lists the trust, naming that one labeler — never a
    # second anonymous mail row.
    short = labeler_id.hex()[:12] + "\u2026"
    app.linked_nests.navigate()
    assert app.nest_trust.wait_for_grant_count(1), (
        f"the labeler grant never rendered on the Nests page; error={app.error_text()!r}"
    )
    scope_text = app.nest_trust.grant_scope_text(0)
    assert S.nests.scope_mail_labeler(labeler=short) in scope_text, (
        f"the Now lens must name the labeler the mail read is confined to; got {scope_text!r}"
    )
    assert S.nests.scope_labeler_labels(labeler=short) in scope_text, (
        f"…and the label-write it licenses; got {scope_text!r}"
    )

    # ── Unsubscribe = revoke: the nest row is gone, the link with it, and
    # History holds the mint beside the revoke.
    cat.navigate_catalog()
    index = cat.find_index_by_factor(factor)
    assert index is not None
    cat.unsubscribe(index)
    assert cat.wait_for_subscribed_state(index, subscribed=False), (
        f"unsubscribe did not flip the row back; error={app.error_text()!r}"
    )
    assert not app.has_error(), f"unexpected error after unsubscribe: {app.error_text()!r}"
    _wait_on_nest(
        lambda: _owner_grant_rows(nest), lambda r: r == [],
        "the labeler grant revoked on the nest",
    )
    assert _subscription_grant_id(nest, labeler_id) is _NO_ROW, "the subscription row is dropped"

    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible()
    _wait_on_nest(
        app.nest_trust.grant_count, lambda n: n == 0,
        "the Now lens no longer lists the revoked labeler trust",
    )
    app.nest_trust.show_history()
    texts = app.nest_trust.history_texts()
    revoked_prefix = S.nests.history_revoked(scope="", when="").split(" \u00b7 ")[0]
    minted_prefix = S.nests.history_minted(scope="", when="").split(" \u00b7 ")[0]
    assert texts and texts[0].startswith(revoked_prefix) and short in texts[0], (
        f"the newest History row must be this labeler's revoke, got {texts!r}"
    )
    assert any(t.startswith(minted_prefix) and short in t for t in texts), (
        f"History keeps the mint beside the revoke, got {texts!r}"
    )


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.feature("community-labelers")
def test_labeler_empty_states_mark_a_loaded_page_not_a_loading_one(logged_in_app):
    """A fresh account's Personalization home finishes loading and SAYS it has
    no subscribed labelers, under ``personalization-labelers-empty``
    (``docs/goal/ui/README.md`` § *List pages: loading is not empty*).

    Why the gate exists: ``entries`` is empty both before the first
    ``fauna.labelers.list`` returns and after one that found nothing, so every
    app announced "You haven't subscribed to any community labelers" over a
    catalog nobody had read. ``LabelerCatalogSnapshot.loaded`` is the second
    painting condition on both surfaces this one machine feeds.

    Scope split, deliberately, mirroring the media precedent: the NEGATIVE half
    — that a page still loading paints no empty state — is a race at this level,
    so it is pinned deterministically one tier down
    (``fauna-labeler-catalog-machine``'s
    ``a_page_that_has_not_refreshed_is_not_a_loaded_empty_page`` and tui's
    ``neither_page_paints_its_empty_state_before_the_read_resolves``, both of
    which fail if the gate is removed).

    What THIS test is the only witness to: that the gate did not break the
    genuine empty state. An app that adds ``loaded &&`` to the paint condition
    but never wires the field through would make the empty state vanish forever
    — a worse bug than the one being fixed, invisible to a tier_1 test running
    against a fake API, and caught here the moment ``wait_for_loaded`` times out.

    Marked for the six apps that gate it today; android is a
    one-line render gate still owed, tracked in
    ``content-moderation-and-ranking.md`` § Implementation status today (leg 5).
    """
    app = logged_in_app
    cat = app.labeler_catalog
    cat.navigate_home()

    loaded = cat.wait_for_loaded("personalization-labelers-empty")
    assert loaded, (
        "the Personalization home never reported itself loaded within "
        f"{cat.LOAD_BUDGET_S:.0f}s: no subscribed rows and no "
        f"personalization-labelers-empty. error={app.error_text()!r}; "
        f"{app.driver.diagnose('personalization-labelers-empty')}"
    )

    assert cat.subscribed_count() == 0, (
        f"fixture account should subscribe to nothing, got {cat.subscribed_count()}"
    )
    assert cat.is_subscribed_empty_visible(), (
        "a loaded page holding zero subscribed labelers must paint "
        "personalization-labelers-empty — the empty state is the loaded-and-empty "
        "half of the three-state rule, not something the gate may suppress"
    )
    assert not app.has_error(), f"unexpected error on personalization home: {app.error_text()!r}"


@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.feature("community-labelers")
def test_personalization_feeds_and_muted_words_links(logged_in_app):
    """``personalization-feeds-link`` exits Settings to the Feed page;
    ``personalization-muted-words-link`` switches this same settings shell to
    the already-shipped muted-words sub-page (reused, not rebuilt)."""
    app = logged_in_app
    app.labeler_catalog.navigate_home()

    app.click("personalization-muted-words-link")
    assert app.muted_words.is_page_visible(), (
        "muted-words-link did not switch to the muted-words sub-page"
    )

    app.labeler_catalog.navigate_home()
    app.click("personalization-feeds-link")
    assert app.feed.is_visible(), "feeds-link did not exit Settings to the Feed page"
