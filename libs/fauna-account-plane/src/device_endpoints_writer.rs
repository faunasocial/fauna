//! The production `fauna.state.device-endpoints` writer — the discovery feed
//! the peer leg reads (`account-data-plane.md` § The peer leg → *Discovery —
//! the replica is its own peer registry* (T5); the consumer is
//! `fauna_peer_sync::discovery::sibling_dial_targets`).
//!
//! Each full pump pass ensures this device's own entry — logical key = the
//! writer id hex, value = [`DeviceEndpoints`] — is published through the
//! fleet plane's writer door. The kind is fleet-only + `GenerationTip`, so
//! the account's **first** publish here is what trips the mint protocol's
//! trigger (a) at the door: the plane resolves no candidate tip, mints
//! through the real escrow doors, and the origination proceeds — no minting
//! happens in this module, which stays a plain door caller.
//!
//! # What publishes when
//!
//! - **First pass, or the transport facts or the enrolled row changed:**
//!   publish. With no facts
//!   supplied yet (no bound listener before W5 (account-data-plane.md § Workstreams), no relay URL learned), the
//!   floor row is `node_id` alone — honest ("this device exists, no dial
//!   paths"), and it is what makes a production account reach its first
//!   generation the moment the runtime first pumps.
//! - **Value unchanged but the resolved tip has superseded the one the row
//!   was published under:** publish again — the retained-window **re-seal**,
//!   scoped to this kind. Without it a later-enrolled device could never
//!   open the first device's row: generation 1 is minted with a member set
//!   of the founding device alone, sealing resolves at seal time, and an
//!   already-published envelope stays as published. The check is exact and
//!   stateless: a v2 wire item key derives from the per-generation schedule,
//!   so "our own relay plane holds a row at the current tip's item key" is
//!   precisely "our row is sealed under the current tip".
//! - **Value unchanged, no tip resolves:** nothing to do — the row (if any)
//!   cannot be re-sealed, and a publish would be refused at the same door.
//! - **No tip resolves, no escrow holder trusted:** skip quietly
//!   ([`EndpointsPass::Unmintable`]). This is the fail-safe posture
//!   (`AccountRuntimeParams::trusted_escrow_holders` — "empty is honest"):
//!   attempting the put would run the whole mint sequence to a
//!   post-deposit refusal on every pass, spamming the escrow door for a
//!   receipt nobody accepts.
//!
//! # The row statement
//!
//! The plane entry is a [`DeviceEndpointsEntry`] — the dial candidates plus
//! this device's own statement of the nest `sync_devices` row it enrolled on
//! (the registration latch's row half). That statement is the client-held
//! truth the devices page's removal resolves its target from
//! (`fauna_core::fleet_removal`): the nest cannot seal this kind, so it cannot
//! re-pair a roster row with another member's principal. It rides here, not
//! on the device-set enrollment, because it is *mutable* — an adopt moves a
//! machine onto a co-located agent's row — and this kind is whole-record LWW
//! per device, where the enrollment's join is not. Plane-only: the carried
//! copy ([`endpoints_of`]) reaches peers outside the fleet and never has it.
//!
//! # Where the facts come from
//!
//! [`EndpointFacts`] is transport truth only the assembler can observe — the
//! bound listener's addresses (W5, rider (a)) and the relay URL the nest
//! advertises (`NestInfoReply.iroh_relay_url`). Apps feed it through
//! `fauna_sync_engine::account_runtime::AccountStoreHandle::set_endpoint_facts`;
//! the next full pass publishes.
//! No human chooses any of it (product-invariant bucket (1)) — there is no
//! app knob here, only observed wiring.

use anyhow::Result;
use ed25519_dalek::SigningKey;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::device_endpoints::{DeviceEndpoints, DeviceEndpointsEntry};
use fauna_core::generation::AdmissibleTip;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_DEVICE_ENDPOINTS;

use crate::account_state_plane::{AccountStatePlane, put_lww_row};
use crate::generation_reclaim::own_row_at;
use crate::generation_tip::{self, GenerationTrust};

/// Transport facts for this device's entry — everything in
/// [`DeviceEndpoints`] except `node_id`, which is the writer identity and
/// never the caller's to supply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointFacts {
    /// Current LAN socket addresses, most-preferred first.
    pub lan_addrs: Vec<String>,
    /// Last-known public (reflexive) socket addresses, most-preferred first.
    pub public_addrs: Vec<String>,
    /// The relay URL this device's nest advertises, when one exists.
    pub relay_url: Option<String>,
}

/// What one ensure step did (the pump's `device_endpoints` report slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointsPass {
    /// The published row already carries the desired value under the current
    /// tip (or no tip resolves and there is nothing to re-seal).
    Current,
    /// A row went through the door: first publish, changed facts, or the
    /// re-seal under a superseding tip.
    Published,
    /// No tip resolves and no escrow holder is trusted — sealing is
    /// refused-by-design on this replica, so the step skips without error.
    Unmintable,
}

/// This device's endpoints value built from its transport facts — the ONE
/// mapping from [`EndpointFacts`] to the published/carried shape.
///
/// Two consumers share it deliberately: the plane entry this module publishes
/// (fleet-only, so siblings learn each other's addresses) and the admit
/// exchange's per-session re-exchange (T13 step 4), which is how the same
/// truth reaches a peer that structurally cannot read the fleet-only kind.
/// `node_id` is the writer identity and never the caller's to supply.
pub fn endpoints_of(device_id: [u8; 32], facts: Option<&EndpointFacts>) -> DeviceEndpoints {
    DeviceEndpoints {
        node_id: device_id,
        lan_addrs: facts.map(|f| f.lan_addrs.clone()).unwrap_or_default(),
        public_addrs: facts.map(|f| f.public_addrs.clone()).unwrap_or_default(),
        relay_url: facts.and_then(|f| f.relay_url.clone()),
    }
}

/// Ensure this device's `fauna.state.device-endpoints` entry is published and
/// sealed under the current tip. Module docs own the decision table.
pub async fn ensure_published<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    facts: Option<&EndpointFacts>,
    enrolled_row: Option<String>,
) -> Result<EndpointsPass>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let device_id = writer_key.verifying_key().to_bytes();
    let key_hex = fauna_core::hex32::encode(&device_id);
    let desired = DeviceEndpointsEntry::new(endpoints_of(device_id, facts), enrolled_row);
    let desired_bytes = fauna_core::encoding::canonical_encode(&desired)?;

    let unchanged = matches!(
        store.state(KIND_DEVICE_ENDPOINTS, &key_hex).await?,
        Some(row) if !row.tombstone && row.value == desired_bytes
    );

    let resolution =
        generation_tip::resolve_tip(store, trust, writer_key, fleet.generation_custody()).await?;
    let Some(tip) = resolution.tip else {
        if unchanged {
            return Ok(EndpointsPass::Current);
        }
        if trust.trusted_holders.is_empty() {
            tracing::debug!(
                "device-endpoints: no tip resolves and no escrow holder is trusted — \
                 fleet-only sealing is refused-by-design on this replica; not publishing"
            );
            return Ok(EndpointsPass::Unmintable);
        }
        // A mint has a real chance (target + holder + door are the mint
        // sequence's own business): go through the door, which owns
        // trigger (a). A refusal (offline) is retried next pass.
        put_lww_row(
            fleet,
            KIND_DEVICE_ENDPOINTS,
            &key_hex,
            desired_bytes,
            device_id,
        )
        .await?;
        return Ok(EndpointsPass::Published);
    };

    if unchanged && published_under(store, &tip, writer_key, &key_hex, fleet).await? {
        return Ok(EndpointsPass::Current);
    }
    put_lww_row(
        fleet,
        KIND_DEVICE_ENDPOINTS,
        &key_hex,
        desired_bytes,
        device_id,
    )
    .await?;
    Ok(EndpointsPass::Published)
}

/// Is our own row published sealed under `tip`? Exact and stateless: the v2
/// wire item key derives from the tip's per-generation schedule, and our own
/// relay plane keeps the live row per `(scope, writer, item)` — so presence
/// at the current tip's item key is the whole answer.
async fn published_under<B: StoreBackend, R: RpcRequester>(
    store: &AccountStore<B>,
    tip: &AdmissibleTip,
    writer_key: &SigningKey,
    key_hex: &str,
    fleet: &AccountStatePlane<'_, B, R>,
) -> Result<bool> {
    let gen_key =
        match generation_tip::key_for_tip(store, tip, writer_key, fleet.generation_custody()).await
        {
            Ok(k) => k,
            Err(e) => {
                // Candidacy includes observer-keyability, so a resolved tip this
                // device cannot key is unexpected — and a put now would fail at
                // the same seal. Hold the row as-is rather than churn.
                tracing::warn!("device-endpoints: cannot key the resolved tip ({e:#}) — holding");
                return Ok(true);
            }
        };
    Ok(
        own_row_at(store, fleet, &gen_key, KIND_DEVICE_ENDPOINTS, key_hex)
            .await?
            .is_some(),
    )
}
