"""tier_3: total-box-loss recovery — the deployment-seed custody loop vs. a real nest.

`docs/goal/architecture/nest/box-recovery.md` § Mechanism (claim-an-existing-box
path) + § Single-identity unification. At claim, the nest hands its deployment
signing seed to the admin in `ClaimAdminReply.deployment_seed` (64-char hex) — a
field the client no longer reads since the plane-era consumer cut (§ The
plane-era recovery floor, (c) The writes): the client's custody leg fetches the
same seed over `fauna.admin.deployment_seed.get` and custodies it off-box on the
account plane (`fauna.state.deployment-seeds`).

The custody WRITE is unit/integration-proven in Rust (`fauna-client-config`'s
`custody_leg` tests + every host's store-ready / post-auth wiring), so this tier_3 test does
NOT re-prove it (and deliberately avoids reimplementing the BackupKey seal/unseal
in Python). What ONLY a real nest can prove — and what BR-2's client-side verify
*depends on* — is that the seed the nest hands off at claim DERIVES TO the box's
advertised identity:

    ed25519(claim_reply.deployment_seed).public  ==  fauna.nest.info → nest_id
                                                  ==  the channel-binding nest_actor_id clients TOFU-pin

The single-identity unification (the commit retiring `nest_identity.key`) makes
`nest.info`'s `nest_id` derive from the same deployment seed, so this equality
holds. If it ever broke — a unification regression, or the claim handler grabbing
the wrong key — the custody leg's seed→bound-id refusal
(`fauna_client_config::run_deployment_seed_custody_leg`) would refuse *every*
legitimate box and recovery custody would silently never happen. This test is
that regression guard; it pairs with the BR-2 verify the custody leg carries.

Wire contracts (verified 2026-06-29):
- `fauna_protocol::claim::ClaimAdminReply.deployment_seed: Option<String>` (hex),
  populated by `bins/fauna-nest/src/claim_handlers.rs` from `nest_signing_key`.
- `fauna_protocol::discovery::NestInfoReply.nest_id: String` (hex), built by
  `discovery_core::nest_info_core` from `state.nest_identity.public_key_bytes()`,
  itself `NestIdentity::from_seed` of the reconciled deployment seed
  (`bins/fauna-nest/src/lib.rs` `start_server`).
"""

from nacl.signing import SigningKey

from tests.api import ws_api

import pytest

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def box_recovery_nest(request, nest_mode, tmp_path_factory):
    """The ORIGINAL box: a claimed nest whose claim reply carries the deployment
    seed. Zero start options — the claim is the harness's own, in every mode."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "box-a")
    yield nest
    cleanup()


@pytest.fixture()
def rebuilt_box(request, nest_mode, tmp_path_factory):
    """Provision a REBUILT box — a brand-new data directory at a new address,
    left unclaimed, exactly as a freshly-provisioned box is before the admin
    re-claims it. Optionally with a custodied deployment seed installed at
    provision time.

    ⚠ **A factory rather than a plain fixture, and its two call shapes are
    spelled out rather than parameterised — that is the point, not verbosity.**
    Which options this journey needs is decided at RUNTIME (the seed comes from
    the original box's claim reply), while `FIXTURE_START_OPTIONS` is derived
    from the kwargs a fixture *literally* passes. A factory forwarding an options
    dict would ask the provider for `extra_env` with the classifier unable to see
    it — trading a collection-time declared absence for a setup-time
    `NestModeError`, which is the outcome this arm's ordering exists to prevent
    (`testing.md` § Default app and nest mode, ruling (3)). Written this way,
    both `unclaimed` and `extra_env` are literal kwargs and the table grades them.

    `FAUNA_DEPLOYMENT_SEED` is catalogued artifact-set IPC (`installers/docker.md`
    § Environment Variables), so a container honours it through the very channel
    cloud-init writes for a real re-provision — which is what makes "provision a
    box with a caller-supplied deployment seed" faithful here rather than merely
    simulated.
    """
    from conftest import _start_dedicated_nest

    started = []

    def _provision(label, seed_hex=None):
        if seed_hex is None:
            nest, cleanup = _start_dedicated_nest(
                request, nest_mode, tmp_path_factory, label, unclaimed=True)
        else:
            nest, cleanup = _start_dedicated_nest(
                request, nest_mode, tmp_path_factory, label, unclaimed=True,
                extra_env={"FAUNA_DEPLOYMENT_SEED": seed_hex})
        started.append(cleanup)
        return nest

    yield _provision
    for cleanup in reversed(started):
        cleanup()


def _derived_pub_hex(seed_hex: str) -> str:
    """`ed25519(seed).public` as hex — the `nest_actor_id` a deployment seed
    derives to. Mirrors `ActorKeypair::from_secret(seed).actor_id()` in shared
    Rust (the exact derivation the custody leg's BR-2 check verifies)."""
    return bytes(SigningKey(bytes.fromhex(seed_hex)).verify_key).hex()


@pytest.mark.feature("recover-a-lost-nest")
def test_claim_handoff_seed_derives_to_nest_info_identity(box_recovery_nest):
    """The deployment seed handed off at claim derives to the box's `nest.info`
    identity (== the pinned `nest_actor_id`) — the load-bearing BR-2 invariant,
    proven against whatever real nest this run's mode serves."""
    nest = box_recovery_nest
    port_box_recovery = nest["port"]
    seed_hex = nest["admin"]["deployment_seed"]

    # Step-2 nest-side: a recovery-capable nest hands the seed off at claim.
    assert seed_hex, "claim reply must carry a deployment_seed (box-recovery step 2)"
    assert len(seed_hex) == 64, (
        f"deployment_seed must be a 64-char hex Ed25519 seed, got {len(seed_hex)}"
    )
    bytes.fromhex(seed_hex)  # must be valid hex

    node_id = ws_api.nest_info(port_box_recovery)["nest_id"]

    # The crux (BR-2 + single-identity unification): the handoff seed derives
    # to the box's advertised identity, so the client's capture-time verify
    # accepts it (never RefusedMismatch) and a recovered box re-presents the
    # SAME nest_actor_id every TOFU-pinned client reconnects to.
    assert _derived_pub_hex(seed_hex) == node_id, (
        "claim-handoff deployment_seed must derive to nest.info nest_id "
        "(the pinned nest_actor_id) — else recovery would mint a DIFFERENT "
        "identity and every pinned client would reject the rebuilt box"
    )

    # Cross-check the harness + unification: `start_nest` reads the same
    # identity off the durable `nest_deployment.key` file, and it agrees with
    # both the claim-handoff seed and nest.info.
    assert nest["nest_id"] == node_id, (
        "nest_deployment.key-derived id must equal nest.info nest_id "
        "(single-identity unification)"
    )


@pytest.mark.feature("recover-a-lost-nest")
def test_rebuilt_box_readopts_custodied_seed_identity(box_recovery_nest, rebuilt_box):
    """**The goal's assertion (1), proven against real nest binaries.**

    `box-recovery.md` § Goal: after total box loss "the rebuilt box re-presents the
    same `nest_actor_id`, so every app that TOFU-pinned it (and any public
    domain's DNS `self=`) reconnects without a trust break."

    This is the **KNOWN-seed inject loop** § Implementation status (the cloud-path
    generate+inject bullet) records as NOT yet built: *"the e2e bridge's `POST
    /nest/start` does not accept arbitrary env, so it cannot inject
    `FAUNA_DEPLOYMENT_SEED`"*. The `rebuilt_box` factory's `extra_env` is that
    affordance, and it reaches a container through the artifact's own env channel
    rather than only a local process's.

    Why no cloud provider is needed: § Mechanism — Restore names the unifying
    primitive as *"provision a box with a caller-supplied deployment seed"*, and
    `FAUNA_DEPLOYMENT_SEED` is the one channel every origin funnels into (cloud-init
    `environment:` for a client-provisioned VPS; the deploy `.env` for both
    self-hosted installers). The provisioner is merely the *transport* for that env
    var, so injecting it directly exercises the identical nest-side adoption path
    (`deployment_key::deployment_seed_from_env` → `reconcile_deployment_keypair`)
    that a real re-provision drives. What this does NOT cover is the cloud
    provider's own API drive (`provision_with_snapshot`) — that needs a
    mock-provisioner seam and is tracked separately.

    Total loss is modelled faithfully: the rebuilt box gets a **brand-new data
    directory** (no `nest.db`, no `blobs/`, and crucially no surviving
    `nest_deployment.key`) on a **new port** — a new VPS at a new address — and is
    left `unclaimed`, exactly as a freshly-provisioned box is before the admin
    re-claims it.

    The no-seed control is what makes the assertion falsifiable: without the env the
    rebuilt box mints a *fresh random* identity, so a green first assertion cannot be
    an artifact of the harness reusing a data dir or the env being silently ignored.
    """
    original = box_recovery_nest
    try:
        seed_hex = original["admin"]["deployment_seed"]
        original_id = original["nest_id"]
        assert seed_hex and original_id
        assert _derived_pub_hex(seed_hex) == original_id
    finally:
        # Total box loss: the VPS is deleted. Its data dir (`tmp_path/box-a`) is
        # never reused below — the rebuilt box starts from nothing but the seed the
        # admin custodied off-box (on the account plane).
        original["proc"].kill()
        original["proc"].wait()

    # Restore: re-provision a fresh box with the custodied seed installed at
    # provision (§ Mechanism — Restore). Unclaimed, like any newly-provisioned box.
    rebuilt = rebuilt_box("box-a-rebuilt", seed_hex=seed_hex)
    port_rebuilt_box = rebuilt["port"]
    assert rebuilt["nest_id"] == original_id, (
        "the rebuilt box must adopt the custodied deployment seed as its durable "
        "identity (nest_deployment.key) — else every TOFU-pinned client rejects "
        "the next channel binding as an identity change and never reconnects"
    )
    # The identity a pinned client actually checks, over the wire.
    assert ws_api.nest_info(port_rebuilt_box)["nest_id"] == original_id, (
        "the rebuilt box must ADVERTISE the original nest_actor_id on nest.info "
        "(the pinned identity + the DNS `self=` comparand)"
    )

    # Falsifiability control: the same fresh-box recipe WITHOUT the seed mints a new
    # random identity. Proves the equality above is caused by the injected seed.
    no_seed = rebuilt_box("box-a-no-seed")
    assert no_seed["nest_id"] != original_id, (
        "a rebuilt box with NO injected seed must mint a fresh random identity — "
        "if this matches, the test is not actually exercising the seed injection"
    )
