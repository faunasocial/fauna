"""tier_3 E2E: bidirectional client↔nest WIRE version-skew grid (I2 wire half).

The wire half of the version-compatibility support matrix
(``docs/goal/architecture/version-compatibility.md`` **Dimension 6** + **§ 5
backlog item 5**). I2 (§ 1, line 35) requires *full bidirectional* compatibility
within a major version — a client may be older OR newer than the nest — and I4
(line 52) names the mechanism for the wire surface verbatim:

    new fields are additive and absorbed by the ``extra`` catch-all on old
    peers; new kinds are tolerated as ``fauna.protocol.unknown_kind`` / the
    ``Unknown`` push envelope; nothing is removed or renamed in place within a
    major.

This grid asserts that mechanism end-to-end against a REAL ``fauna-nest`` binary,
using the raw-DAG-CBOR WS-RPC clients as a *skew simulator*: an old client is one
that sends a field-subset payload and never names a newer kind; a new client is
one that sends extra unknown fields and names kinds an older nest never
registered. The current binary is a faithful stand-in for *both* version endpoints
because the two behaviors under test — serde ignore-unknown / ``extra`` catch-all,
and the router's ``unknown_kind`` miss — are version-independent: an *older* nest
that carried the catch-all ignores a *newer* client's added fields exactly as the
current nest ignores a genuinely-unknown field, and answers ``unknown_kind`` to a
kind it never registered exactly as the current nest answers a made-up future
kind. (Wire skew has no Docker-image dimension, so this is tier_3, not tier_4: a
single built image cannot be both "old client" and "new nest"; the crafted-payload
client behaves identically against a binary or an image, so the image adds no
coverage — tier decision tracked internally.)

Three cells:

- **old client → new nest** (forward-compat, I2 line 42): the current (new) nest
  accepts a *minimal / baseline-only* request, filling serde defaults for any
  field a newer client would add. Proven by the discovery smoke kinds accepting an
  empty ``{}`` payload — the nest requires nothing an old client wouldn't send.
- **new client → old nest, extra fields** (backward-compat): a *known* kind sent
  with junk extra fields is processed identically to the clean call — the unknown
  keys are ignored (the universal ``extra`` catch-all / serde ignore-unknown). An
  old nest tolerates a new client's added fields.
- **new client → old nest, unknown kind** (backward-compat): a kind the router
  never registered is answered with the typed ``fauna.protocol.unknown_kind`` and
  the connection stays open (a later real call still succeeds) — never a crash, a
  hang, or a dropped connection. This is the runtime counterpart of the
  ``rpc_unknown_kind_observed_total`` fork metric and the static
  ``tools/check-additive-evolution`` lint.

The at-rest half of the matrix (old-binary-opens-new-DB) lives next door in
``test_schema_version_compat.py`` (``test_newer_but_additive_schema_still_serves``
+ ``test_old_binary_serves_new_db_with_extra_table``); the image-level upgrade /
degraded-boot gates are the tier_4 ``test_mail_deploy_schema_upgrade.py``.

These are characterization/regression tests of an already-built contract
(additive-everywhere is live), so they pass today; their value is locking the
contract — a future non-additive wire regression turns one RED.
"""

import re

import pytest


from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient

pytestmark = pytest.mark.tier_3


# Build/version capability tokens this binary's build advertises
# (``fauna_protocol::discovery::capability`` — all four families are
# always-compiled in this major version). The Python mirror of the Rust constants;
# a coarse feature name per token.
EXPECTED_BASELINE_CAPABILITIES = {"mail", "calendar", "subscriptions", "file_sync"}
# A coarse token is a short lowercase feature name — never a patch/build
# fingerprint (the Dim 3 anti-fingerprint constraint; the anonymous reply already
# coarsens ``version`` to ``major.minor``): lowercase alphanumeric words joined by
# single ``-`` or ``_``, so ``1.2.3``, ``v1.2.3+build``, ``Nest`` and any
# dotted/doubled/trailing-separator shape are all rejected.
#
# ⚠ 2026-08-20: this pattern used to be
# ``^[a-z][a-z0-9_]*$`` — underscores only — and it was the TEST that was wrong,
# not the code. Two reasons, in order of weight:
#
#   1. The ratified constraint is COARSENESS, not a character set.
#      ``version-compatibility.md`` § Dimension 3 says only "tokens are coarse
#      feature names (mail/calendar/subscriptions/file_sync), never a patch/build
#      fingerprint". It prescribes no separator; the underscore-only reading was
#      generalized from those four examples, and ``spam-model-sealed-at-rest`` is
#      as coarse a feature name as ``file_sync`` is.
#   2. These tokens are PUBLISHED WIRE VALUES. Several of the advertised
#      constants use hyphens (``spam-model-sealed-at-rest``, ``peer-sync``,
#      ``hidden-tiers``), each shipped alongside a real
#      client consumer that gates on the exact string. Renaming one to satisfy a
#      test regex would break a client that already gates on it — a within-major
#      compatibility break (``version-compatibility.md``: additive-everywhere,
#      full bidirectional client↔nest compat within a major), traded for nothing.
#
# Convention for NEW tokens (de-facto, 4:1 among the multiword ones): hyphens.
# ``file_sync`` keeps its underscore because it, too, is already on the wire.
_COARSE_TOKEN = re.compile(r"^[a-z][a-z0-9]*([_-][a-z0-9]+)*$")


@pytest.fixture(scope="module")
def skew_nest(request, nest_mode, tmp_path_factory):
    """A single claimed nest (the 'new nest' / 'old nest' endpoint under test).

    Claimed (not ``unclaimed``) so the authenticated ``WsRpcAdminClient`` cell
    (2b — unknown kind reaches the router, past the anon pre-identity gate) has a
    real admin identity. No storage mode is committed — the discovery kinds this
    grid exercises (``fauna.nest.info`` / ``fauna.setup.status``) don't gate on it.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "wire_skew")
    yield nest
    cleanup()


def _admin_client(nest) -> WsRpcAdminClient:
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


# ── Cell 1 — old client → new nest (forward-compat) ─────────────────────────


@pytest.mark.feature("upgrades-never-lose-data")
def test_old_client_minimal_payloads_accepted_by_new_nest(skew_nest):
    """An OLD client sends only baseline ``{}`` payloads (it knows none of the
    optional fields a newer client adds). The current (NEW) nest must accept them,
    filling serde defaults — it requires nothing an old client wouldn't send."""
    with WsRpcAnonClient(skew_nest["url"]) as anon:
        status = anon.call("fauna.setup.status", {})
        assert isinstance(status, dict) and "claimed" in status, (
            f"old-client minimal fauna.setup.status must succeed; got {status!r}"
        )
        info = anon.call("fauna.nest.info", {})
        assert isinstance(info, dict) and "version" in info, (
            f"old-client minimal fauna.nest.info must succeed; got {info!r}"
        )


# ── Cell 2a — new client → old nest: unknown extra fields ignored ────────────


@pytest.mark.feature("upgrades-never-lose-data")
def test_new_client_extra_fields_ignored_by_old_nest(skew_nest):
    """A NEW client adds fields an OLD nest never defined. The nest must IGNORE
    them (the universal ``extra`` catch-all / serde ignore-unknown) and return the
    SAME observable reply as the clean call — never ``fauna.protocol.malformed``."""
    with WsRpcAnonClient(skew_nest["url"]) as anon:
        clean = anon.call("fauna.setup.status", {})
        with_extra = anon.call(
            "fauna.setup.status",
            {
                "_future_capability_flag": True,
                "_unknown_int_field": 7,
                "_nested_future_obj": {"added_in": "a-newer-client", "n": 1},
                "_future_list": [1, 2, 3],
            },
        )
        assert with_extra == clean, (
            "extra unknown fields must be ignored — the reply must match the clean "
            f"call (I4 wire catch-all); clean={clean!r} with_extra={with_extra!r}"
        )

        # And on a kind with a required field: the required field is honored, the
        # extra ones ignored — not a malformed rejection.
        avail = anon.call(
            "fauna.handle.available",
            {"handle": "skewtest", "_client_added_field": "ignored"},
        )
        assert isinstance(avail, dict), (
            f"fauna.handle.available with an extra field must still resolve; got {avail!r}"
        )


# ── Cell 2b — new client → old nest: unknown kind degrades gracefully ────────


@pytest.mark.feature("upgrades-never-lose-data")
def test_new_client_unknown_kind_gets_graceful_unknown_kind(skew_nest):
    """A NEW client names a kind an OLD nest never registered. The nest must answer
    the typed ``fauna.protocol.unknown_kind`` (NOT a raw error, NOT a hang) AND keep
    the connection open — a later real call on the same connection still succeeds.

    Uses the AUTHENTICATED client on purpose: the anonymous connection gates any
    off-allowlist kind with ``fauna.protocol.unauthenticated`` *before* the router,
    so only an authenticated connection reaches the router miss that produces
    ``unknown_kind`` (``routes.rs::dispatch_request``)."""
    with _admin_client(skew_nest) as admin:
        with pytest.raises(RpcCallError) as exc:
            admin.call("fauna.future.capability_does_not_exist_yet", {"some": "args"})
        assert exc.value.code == "fauna.protocol.unknown_kind", (
            "an unknown (future) kind must get the typed fauna.protocol.unknown_kind "
            f"so an old nest degrades gracefully for a new client; got {exc.value.code!r} "
            f"(details={exc.value.details!r})"
        )

        # The connection survived the unknown-kind miss (the dispatcher keeps it
        # open so one stray request can't kill an in-flight bootstrap): a real call
        # still works.
        info = admin.call("fauna.nest.info", {})
        assert isinstance(info, dict) and "version" in info, (
            "the connection must stay open after an unknown-kind reply; the next "
            f"real call must succeed; got {info!r}"
        )


# ── Cell 3 — capability set (version-compatibility Dimension 3) ──────────────


@pytest.mark.feature("upgrades-never-lose-data")
def test_nest_info_advertises_capability_set(skew_nest):
    """The anonymous ``fauna.nest.info`` reply carries a ``capabilities`` list — the
    build/version feature-capability set a newer client reads to hide a feature an
    older nest predates (``version-compatibility.md`` § Dimension 3). This is the
    NEW-nest-advertises half of the skew matrix; the OLD-nest-omits half (an absent
    field decodes to ``vec![]`` → every token reads unsupported) is the tier_1
    ``fauna-protocol`` ``nest_info_reply_from_old_nest_without_capabilities_reads_unsupported``.

    Asserts the wire contract: a list of *coarse* tokens (anti-fingerprint), the
    baseline families present, and — crucially for the degrade — that a made-up
    *future* capability is ABSENT. The current nest is a faithful skew stand-in for
    "an older nest lacking feature X": any token it does not advertise is exactly
    what a client reading an old nest sees, and the contract is that the client
    treats it as unsupported (hides the feature) rather than erroring.
    """
    with WsRpcAnonClient(skew_nest["url"]) as anon:
        info = anon.call("fauna.nest.info", {})
        caps = info.get("capabilities")
        assert isinstance(caps, list) and all(isinstance(c, str) for c in caps), (
            f"fauna.nest.info must carry a `capabilities` list of strings; got {caps!r}"
        )

        # Coarse tokens only — no patch/build fingerprint leaks through the set.
        for c in caps:
            assert _COARSE_TOKEN.match(c), (
                f"capability token {c!r} is not a coarse feature name "
                "(anti-fingerprint, Dim 3 constraint)"
            )

        # The baseline always-compiled families are advertised.
        assert EXPECTED_BASELINE_CAPABILITIES.issubset(set(caps)), (
            "the baseline capability families must be advertised; "
            f"expected ⊇ {EXPECTED_BASELINE_CAPABILITIES}, got {set(caps)}"
        )

        # Degrade contract: a capability this nest does NOT advertise (here a
        # made-up future feature an older nest could never have) is absent — a
        # client reads absence as unsupported and hides the feature.
        assert "fauna_future_feature_not_real" not in caps, (
            "an unadvertised/future capability must be ABSENT so a client reads it "
            f"as unsupported; got {caps!r}"
        )

        # `capabilities` and `protocols` stay distinct namespaces: the federation
        # `protocols` list is NOT polluted with feature-capability tokens (bridge
        # UIs gate on `protocols`, feature UIs on `capabilities`).
        protocols = info.get("protocols", [])
        assert not (EXPECTED_BASELINE_CAPABILITIES & set(protocols)), (
            "feature-capability tokens must not leak into `protocols` (federation "
            f"protocols); protocols={protocols!r}"
        )


# ── Cell 4 — the admin-mail twin, both skew directions ──────────────────────
#
# `fauna.bridges.get_mail_config` and the five `put_<substruct>_policy` writes are
# Admin-class kinds an APP calls: the `admin-mail` page on all 7 apps hydrates
# from the read twin and saves through the writes
# (``docs/goal/behavior/mail-policy-config.md`` § Implementation status today).
# They live in ``fauna-protocol::bridge_routing``, whose in-image MTA/MDA
# data-plane structs are the one sanctioned ``deny_unknown_fields`` opt-out
# (``transport.md`` § Schema and forward-compat discipline, rule 4) — but the
# opt-out is scoped by WHO DECODES, and these are decoded by an app, so they are
# ordinary client↔nest wire and I2's both-directions requirement applies.
#
# Until 2026-08-27 the whole module was strict, so this surface was skew-broken in
# both directions: nine fields added since alpha began (2026-06-14) made an older
# app's `admin-mail` page fail to hydrate outright against a newer nest, and a
# newer app's save was refused wholesale by an older nest. These two cells are the
# grid's coverage of that surface — it had none.

#: Fields added to `FetchConfigReply`/its policy sub-structs AFTER alpha began
#: (2026-06-14). Every one of them is a key an app older than it never defined —
#: which is exactly why a strict decoder made the page unopenable. Kept as data so
#: cell 4b asserts the skew pressure is REAL and not hypothetical.
POST_ALPHA_REPLY_FIELDS = ("caldav_port", "carddav_enabled", "webdav_enabled")


@pytest.mark.feature("upgrades-never-lose-data")
def test_new_app_policy_save_with_future_field_accepted_by_old_nest(skew_nest):
    """**New app → old nest (write).** A NEWER app saves the Spam group carrying a
    knob this nest never defined. The nest must ACCEPT the save — not refuse the
    whole request with ``fauna.protocol.malformed`` — and must still apply the
    knobs it *does* understand, so tolerance is genuinely "ignore the unknown key",
    never "silently drop the request".

    Red before the 2026-08-27 de-strictening: ``PutSpamPolicyRequest`` was
    ``deny_unknown_fields``, so this call failed at decode and an admin running a
    newer app simply could not save policy against an older nest.
    """
    with _admin_client(skew_nest) as admin:
        # A distinctive value on a KNOWN knob, alongside a knob only a future app
        # would send. Both travel in one frame, exactly as a newer app's full-PUT
        # of the Spam group would.
        admin.call(
            "fauna.bridges.put_spam_policy",
            {
                "baseline_standing_publish": False,
                "greylist_delay_secs": 97,
                "_future_spam_knob": "added-by-a-newer-app",
                "_future_nested": {"weight": 3},
            },
        )

        # The save was not merely accepted-and-discarded: the known knob landed.
        config = admin.call("fauna.bridges.get_mail_config", {})
        spam = config.get("spam")
        assert isinstance(spam, dict), (
            f"get_mail_config must carry the spam sub-struct; got {config!r}"
        )
        assert spam.get("greylist_delay_secs") == 97, (
            "the known knob in a frame that ALSO carried an unknown one must take "
            f"effect — tolerance must not swallow the request; got {spam!r}"
        )

        # And the unknown knob is not persisted back out as a policy field: the
        # nest understood it as unknown, it did not invent a setting from it.
        assert "_future_spam_knob" not in spam, (
            "an unknown request key must not become a policy field; "
            f"got {spam!r}"
        )


def test_old_app_reads_mail_config_carrying_post_alpha_fields(skew_nest):
    """**Old app → new nest (read).** The reply this nest sends genuinely carries
    fields added after alpha began — so an app older than them meets unknown keys
    on the hydrate path for real, not hypothetically.

    Scope, stated honestly: the *decoder* half of this direction — that the app's
    typed ``FetchConfigReply`` tolerates and re-emits such a key rather than
    erroring — is a Rust serde property and is pinned where it lives, in the tier_1
    ``fauna-protocol`` tests ``fetch_config_reply_wire_preserves_unknown_fields`` and
    ``fetch_config_sub_struct_wire_preserves_unknown_fields`` (both red-verified by
    restoring ``deny_unknown_fields``). This cell supplies the other half that only
    a real nest can: that the skew pressure those pins absorb actually exists on
    the wire. Faking a nest-side unknown-field injection here would need a new
    production seam for no added coverage, so it is deliberately not done.
    """
    with _admin_client(skew_nest) as admin:
        config = admin.call("fauna.bridges.get_mail_config", {})
        assert isinstance(config, dict), (
            f"the admin read twin must answer a config map; got {config!r}"
        )
        present = [f for f in POST_ALPHA_REPLY_FIELDS if f in config]
        assert present, (
            "the reply must carry at least one post-alpha field, else this cell is "
            "asserting nothing about skew pressure; expected any of "
            f"{POST_ALPHA_REPLY_FIELDS}, got keys {sorted(config)!r}"
        )
