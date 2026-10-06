//! Discovery — the replica is its own peer registry (T5, ratified;
//! `account-data-plane.md` § The peer leg → *Discovery*): dial candidates
//! come from the plane's own `fauna.state.device-endpoints` entries, cached
//! while connectivity lasted. **No broadcast, no scanning, no third-party
//! substrate** — the LAN arithmetic is the shipped [`crate::lan`] math
//! applied to those cached addresses, and every candidate passes the
//! shared PT-4 hygiene filter (`fauna_transport::is_safe_candidate`) before
//! it can be dialed.
//!
//! **Production feed (landed 2026-08-13 with the generation schedule):**
//! `fauna_sync_engine::device_endpoints_writer` — each account runtime's
//! pump publishes this device's own entry through the fleet plane's writer
//! door (generation-sealed; the account's first publish mints via trigger
//! (a)). The kind stays fleet-only by design — generation keying is what
//! severs a removed device from the fleet's future addresses. This module
//! reads whatever entries the store holds; proofs may still stage entries
//! door-lessly (the sanctioned `conformance_account_state_walk.rs` pattern).

use std::net::{IpAddr, SocketAddr};

use crate::lan::should_attempt_lan_probe;
use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::encoding::canonical_decode;
use fauna_protocol::discovery::capability;
use fauna_protocol::merge_policy::KIND_DEVICE_ENDPOINTS;
use fauna_transport::{PathCandidates, is_safe_candidate};

/// The production source of this device's own interface addresses — for the
/// *publish* half of discovery (`EndpointFacts.lan_addrs` composition at the
/// W5.7 (account-data-plane.md § Workstreams) assembly seam) exactly as [`sibling_dial_targets`]'s `our_lan_ips`
/// parameter names it for the dial half. Re-exported here so consumers take
/// one discovery surface instead of reaching into [`crate::lan`]; tests
/// keep injecting instead of calling this (they never touch live interfaces).
pub use crate::lan::discover_lan_candidates;

/// One sibling replica this device could dial: its transport identity plus
/// the candidates the seam's three-path cascade consumes. `relay_url` rides
/// beside them because the relay is configured on the *endpoint builder*
/// (`fauna_iroh::IrohTransportBuilder::relay_url`), not per dial — the
/// assembler feeds it there; `candidates.relay_available` reflects it.
#[derive(Debug, Clone)]
pub struct PeerDialTarget {
    /// The sibling's device-principal key — its NodeId on the seam.
    pub node_id: [u8; 32],
    pub candidates: PathCandidates,
    /// The relay URL the sibling's nest advertises, when one exists.
    pub relay_url: Option<String>,
}

/// The rule-7 client gate: run the peer leg only while the nest advertises
/// the `peer-sync` capability (the fleet brake —
/// `fauna_protocol::discovery::capability::PEER_SYNC`).
pub fn peer_sync_enabled(nest_capabilities: &[String]) -> bool {
    capability::supports(nest_capabilities, capability::PEER_SYNC)
}

/// Sibling dial targets from the store's device-endpoint entries.
///
/// - `own_writer_hex`: this replica's own writer id (its entry is skipped —
///   a device does not dial itself).
/// - `verified_member`: is this id a verified, non-removed member of this
///   replica's fleet view (`fauna_core::generation::FleetView::is_verified_member`
///   in production)? **An entry is a dial candidate only while its device
///   answers true** (`account-sync-plane.md` § The peer leg → *Discovery*):
///   nothing forgets a merged entry, so a removed device's, a signed-out
///   device's and every predecessor device's entry outlives its device, and
///   dialing it would only spend a failing dial and show this device's
///   address to whatever answers at that node id. The admission rule would
///   refuse such a peer anyway; this filter refuses it before the dial.
/// - `our_lan_ips`: this device's own interface addresses
///   ([`discover_lan_candidates`] in production;
///   injected so tests never touch live interfaces). LAN candidates are
///   included only when the shipped arithmetic says a probe could work
///   (RFC-1918 + shared /24 — v4; safe v6 LAN/ULA candidates are kept as-is,
///   the seam's own hygiene filter having no subnet analog for them).
/// - Every address passes [`is_safe_candidate`] (PT-4): loopback,
///   link-local, IMDS, multicast etc. from a hostile entry are dropped, not
///   dialed.
pub async fn sibling_dial_targets<B: StoreBackend>(
    store: &AccountStore<B>,
    own_writer_hex: &str,
    verified_member: impl Fn(&[u8; 32]) -> bool,
    our_lan_ips: &[std::net::Ipv4Addr],
) -> Result<Vec<PeerDialTarget>> {
    let mut targets = Vec::new();
    for entry in store.states_of_kind(KIND_DEVICE_ENDPOINTS).await? {
        if entry.key == own_writer_hex {
            continue;
        }
        let value: DeviceEndpoints = canonical_decode(&entry.value)
            .with_context(|| format!("device-endpoints entry {:?}", entry.key))?;

        // Cross-check the value's node id against the entry's logical key
        // (one entry per device, keyed by its writer id hex — the redundancy
        // exists exactly for this check). A mismatch is a malformed or
        // tampered entry: skip it, never dial it.
        if hex::encode(value.node_id) != entry.key {
            tracing::warn!(
                key = %entry.key,
                "device-endpoints entry whose node_id disagrees with its key — skipped"
            );
            continue;
        }
        if !verified_member(&value.node_id) {
            tracing::debug!(
                key = %entry.key,
                "device-endpoints entry of a device outside the verified fleet — not dialed"
            );
            continue;
        }

        targets.push(dial_target_from(value, our_lan_ips));
    }
    Ok(targets)
}

/// One [`DeviceEndpoints`] value → a dial target, under the seam's shared
/// hygiene (PT-4 [`is_safe_candidate`] on every address; the v4 LAN-probe
/// arithmetic; first safe WAN candidate). The one composition every
/// endpoints consumer uses — sibling dials, the owner-side custodian dials
/// (`custodian-endpoints` rows) and the custodian's owner-fleet dials
/// (`custodies-held` rows' `owner_devices`) — so a hostile entry is filtered
/// identically wherever it arrived from (W8.5).
pub fn dial_target_from(
    value: DeviceEndpoints,
    our_lan_ips: &[std::net::Ipv4Addr],
) -> PeerDialTarget {
    let parse_safe = |addrs: &[String]| -> Vec<SocketAddr> {
        addrs
            .iter()
            .filter_map(|a| a.parse::<SocketAddr>().ok())
            .filter(is_safe_candidate)
            .collect()
    };

    let lan_all = parse_safe(&value.lan_addrs);
    let peer_v4: Vec<std::net::Ipv4Addr> = lan_all
        .iter()
        .filter_map(|a| match a.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .collect();
    let v4_probe = should_attempt_lan_probe(our_lan_ips, &peer_v4);
    let lan_endpoints: Vec<SocketAddr> = lan_all
        .into_iter()
        .filter(|a| match a.ip() {
            IpAddr::V4(_) => v4_probe,
            IpAddr::V6(_) => true,
        })
        .collect();

    let wan_endpoint = parse_safe(&value.public_addrs).into_iter().next();

    PeerDialTarget {
        node_id: value.node_id,
        candidates: PathCandidates {
            lan_endpoints,
            wan_endpoint,
            relay_available: value.relay_url.is_some(),
        },
        relay_url: value.relay_url,
    }
}

/// Bind a peer's *carried* endpoints (the admit exchange's per-session
/// re-exchange slot, T13 step 4) to the identity the channel actually proved.
///
/// The carried `node_id` is **peer-supplied and is not identity** — the slot's
/// own contract calls it "advisory transport truth, never identity: the
/// channel-proven key is the peer's identity regardless of what this names"
/// (`fauna_protocol::peer_sync::PeerSyncAdmitRequest::endpoints`). So a value
/// naming anyone but the proven peer is **refused**, exactly as
/// [`sibling_dial_targets`] skips a plane row whose `node_id` disagrees with
/// its key: a peer must never be able to point a consumer's future dials — or
/// a durable registry row — at a third node it does not control.
///
/// `None` in, `None` out (the slot is optional in both directions).
pub fn bind_carried_endpoints(
    carried: Option<DeviceEndpoints>,
    proven_key: &[u8; 32],
) -> Option<DeviceEndpoints> {
    let value = carried?;
    if &value.node_id != proven_key {
        tracing::warn!(
            named = %fauna_core::hex32::encode(&value.node_id),
            proven = %fauna_core::hex32::encode(proven_key),
            "admit endpoints re-exchange: carried node_id disagrees with the \
             channel-proven key — refused"
        );
        return None;
    }
    Some(value)
}

/// The owner-fleet side's custodian dial targets (W8.5 P5): one per
/// `fauna.state.custodian-endpoints` row — dialed BESIDE the sibling
/// targets, admitted with the same own-fleet `DeviceAuthorization`, walked
/// with the ordinary pull-only walks (a custodied store serves the same
/// relay plane a sibling does).
pub async fn custodian_dial_targets<B: StoreBackend>(
    store: &AccountStore<B>,
    our_lan_ips: &[std::net::Ipv4Addr],
) -> Result<Vec<PeerDialTarget>> {
    let mut targets = Vec::new();
    for entry in store
        .states_of_kind(fauna_protocol::merge_policy::KIND_CUSTODIAN_ENDPOINTS)
        .await?
    {
        let value: fauna_core::custodian_endpoints::CustodianEndpoints =
            canonical_decode(&entry.value)
                .with_context(|| format!("custodian-endpoints entry {:?}", entry.key))?;
        targets.push(dial_target_from(value.endpoints, our_lan_ips));
    }
    Ok(targets)
}

/// The **share leg's** dial targets for one shared file set — one per
/// `fauna.state.share-endpoints` row whose key names `channel_id`
/// (`docs/goal/behavior/p2p.md` § Cross-user shared-set transfer →
/// *Discovery*; the W8 share twin's slice F).
///
/// The third reader of the location-data family, and deliberately the same
/// shape as its two siblings: rows in, [`dial_target_from`] out, so a
/// counterparty's advertised candidates pass **exactly** the hygiene an own
/// sibling's do — PT-4 [`is_safe_candidate`], the shipped v4 LAN-probe
/// arithmetic, first-safe-WAN, relay availability. A hostile advertisement
/// therefore buys no reach that a hostile plane row would not.
///
/// Two differences from the same-account readers, both load-bearing:
///
/// - **`node_id` is the member's ACTOR key**, not a device principal — the
///   share leg dials the contact plane (PT-1b). The composition does not
///   care; the meaning does, and both ends say so
///   (`fauna_core::share_endpoints`).
/// - **Scoped to one set.** A member is admitted per set, so the caller asks
///   per set: rows for other sets are not this set's business, and a
///   consumer must never widen a dial list across the sets it holds.
///
/// A row whose stored `member_actor`/`channel_id` disagrees with its logical
/// key is **skipped, never dialed** — the same cross-check
/// [`sibling_dial_targets`] runs, for the same reason: the redundancy exists
/// precisely so a tampered row cannot point a consumer's dials at a node the
/// advertiser does not control.
pub async fn share_dial_targets<B: StoreBackend>(
    store: &AccountStore<B>,
    channel_id: &[u8; 32],
    our_lan_ips: &[std::net::Ipv4Addr],
) -> Result<Vec<PeerDialTarget>> {
    let rows = store
        .states_of_kind(fauna_protocol::merge_policy::KIND_SHARE_ENDPOINTS)
        .await?;
    share_dial_targets_from_rows(&rows, channel_id, our_lan_ips)
}

/// [`share_dial_targets`] over rows already read — the live
/// `fauna.state.share-endpoints` entries as `AccountStore::states_of_kind`
/// answers them. The account runtime's handle serves that read as a local
/// command and the share pump runs this over the answer, so the walk needs no
/// door of its own on the store thread.
pub fn share_dial_targets_from_rows(
    rows: &[fauna_account_store::types::StateEntry],
    channel_id: &[u8; 32],
    our_lan_ips: &[std::net::Ipv4Addr],
) -> Result<Vec<PeerDialTarget>> {
    let prefix = format!("{}:", fauna_core::hex32::encode(channel_id));
    let mut targets = Vec::new();
    for entry in rows {
        let Some(actor_hex) = entry.key.strip_prefix(&prefix) else {
            continue; // another set's row
        };
        let value: fauna_core::share_endpoints::ShareEndpoints = canonical_decode(&entry.value)
            .with_context(|| format!("share-endpoints entry {:?}", entry.key))?;

        // Both halves of the key are carried in the value on purpose — check
        // both, and check the advertised NodeId against the member the key
        // names. A disagreement is a malformed or tampered row.
        if hex::encode(value.member_actor.as_slice()) != actor_hex
            || hex::encode(value.channel_id.as_slice()) != prefix.trim_end_matches(':')
            || hex::encode(value.endpoints.node_id) != actor_hex
        {
            tracing::warn!(
                key = %entry.key,
                "share-endpoints entry disagrees with its key — skipped"
            );
            continue;
        }

        targets.push(dial_target_from(value.endpoints, our_lan_ips));
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::{StateEntry, WriterId};
    use fauna_core::encoding::canonical_encode;
    use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;

    const OWN: [u8; 32] = [7u8; 32];
    const SIBLING: [u8; 32] = [9u8; 32];

    async fn store_with(entries: &[([u8; 32], DeviceEndpoints)]) -> AccountStore<SqliteBackend> {
        let store = AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            "aa11",
            WriterId(OWN),
        )
        .await
        .unwrap();
        // Door-less staging (the sanctioned conformance pattern): the kind is
        // R14 (account-data-plane.md § The ratified decisions)-gated at the PLANE's writer door; the store API underneath is
        // where proofs stage rows.
        for (writer, value) in entries {
            store
                .put_state(StateEntry {
                    kind: KIND_DEVICE_ENDPOINTS.to_string(),
                    key: hex::encode(writer),
                    scope: ACCOUNT_STATE_SCOPE.to_string(),
                    value: canonical_encode(value).unwrap(),
                    merge_meta: None,
                    entry_version: 0,
                    tombstone: false,
                })
                .await
                .unwrap();
        }
        store
    }

    fn endpoints(node_id: [u8; 32]) -> DeviceEndpoints {
        DeviceEndpoints {
            node_id,
            lan_addrs: vec!["192.168.1.9:4433".into(), "127.0.0.1:4433".into()],
            public_addrs: vec!["203.0.113.9:4433".into()],
            relay_url: Some("https://relay.example/".into()),
        }
    }

    /// The happy path: a sibling's entry becomes a dial target — own entry
    /// skipped, unsafe candidates dropped (PT-4), LAN kept when the shipped
    /// arithmetic says a probe could work, relay URL carried through.
    #[tokio::test]
    async fn sibling_entries_become_dial_targets_with_hygiene_applied() {
        let store = store_with(&[(OWN, endpoints(OWN)), (SIBLING, endpoints(SIBLING))]).await;

        // Same /24 as the sibling's LAN candidate → probe is worth attempting.
        let ours = ["192.168.1.7".parse().unwrap()];
        let targets = sibling_dial_targets(&store, &hex::encode(OWN), |_| true, &ours)
            .await
            .unwrap();

        assert_eq!(targets.len(), 1, "own entry is never a dial target");
        let t = &targets[0];
        assert_eq!(t.node_id, SIBLING);
        assert_eq!(
            t.candidates.lan_endpoints,
            vec!["192.168.1.9:4433".parse::<SocketAddr>().unwrap()],
            "the loopback candidate must be dropped (PT-4), the LAN one kept"
        );
        assert_eq!(
            t.candidates.wan_endpoint,
            Some("203.0.113.9:4433".parse().unwrap())
        );
        assert!(t.candidates.relay_available);
        assert_eq!(t.relay_url.as_deref(), Some("https://relay.example/"));
    }

    /// Different subnet → the shipped arithmetic says a v4 LAN probe is
    /// pointless; the candidates drop out while WAN + relay stay.
    #[tokio::test]
    async fn lan_candidates_drop_when_no_shared_subnet() {
        let store = store_with(&[(SIBLING, endpoints(SIBLING))]).await;
        let ours = ["10.0.0.7".parse().unwrap()];
        let targets = sibling_dial_targets(&store, &hex::encode(OWN), |_| true, &ours)
            .await
            .unwrap();
        assert!(targets[0].candidates.lan_endpoints.is_empty());
        assert!(targets[0].candidates.wan_endpoint.is_some());
    }

    /// An entry is a dial candidate only while its device is a verified
    /// fleet member: the merged entry of a departed device outlives it, and
    /// the predicate is what keeps it off the dial list.
    #[tokio::test]
    async fn an_entry_outside_the_verified_fleet_is_not_a_dial_target() {
        const DEPARTED: [u8; 32] = [0x0Du8; 32];
        let store = store_with(&[
            (SIBLING, endpoints(SIBLING)),
            (DEPARTED, endpoints(DEPARTED)),
        ])
        .await;
        let targets = sibling_dial_targets(&store, &hex::encode(OWN), |id| *id == SIBLING, &[])
            .await
            .unwrap();
        let dialed: Vec<[u8; 32]> = targets.iter().map(|t| t.node_id).collect();
        assert_eq!(dialed, vec![SIBLING]);
    }

    /// A tampered entry whose node_id disagrees with its logical key is
    /// skipped — never dialed.
    #[tokio::test]
    async fn a_key_mismatched_entry_is_skipped() {
        // Staged under SIBLING's key but claiming OWN's node id.
        let store = store_with(&[(SIBLING, endpoints(OWN))]).await;
        let targets = sibling_dial_targets(&store, &hex::encode(OWN), |_| true, &[])
            .await
            .unwrap();
        assert!(targets.is_empty());
    }

    /// T13 step 4's carried endpoints are bound to the CHANNEL-PROVEN key: a
    /// peer naming itself is believed, a peer naming a third node is refused
    /// outright. The refusal is the whole security property of the slot — a
    /// believed `node_id` would let any admitted peer redirect the consumer's
    /// future dials, and (once written back) a durable registry row, at a box
    /// it does not control.
    #[test]
    fn carried_endpoints_bind_to_the_channel_proven_key() {
        // Names itself — believed verbatim.
        assert_eq!(
            bind_carried_endpoints(Some(endpoints(SIBLING)), &SIBLING),
            Some(endpoints(SIBLING)),
            "a peer naming itself carries usable transport truth"
        );
        // Names a THIRD node over a channel proving SIBLING — refused.
        assert_eq!(
            bind_carried_endpoints(Some(endpoints(OWN)), &SIBLING),
            None,
            "a carried node_id that is not the proven peer must be refused"
        );
        // The slot is optional in both directions.
        assert_eq!(bind_carried_endpoints(None, &SIBLING), None);
    }

    // ── The share leg's reader (slice F) ────────────────────────────────

    const SET: [u8; 32] = [0x5E; 32];
    const OTHER_SET: [u8; 32] = [0x5F; 32];
    const MEMBER: [u8; 32] = [0x3B; 32];

    /// Stage share-endpoint rows door-lessly (the sanctioned conformance
    /// pattern — the plane's writer door is R14-gated; proofs stage
    /// underneath it).
    async fn store_with_share_rows(
        rows: &[(String, fauna_core::share_endpoints::ShareEndpoints)],
    ) -> AccountStore<SqliteBackend> {
        let store = AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            "aa11",
            WriterId(OWN),
        )
        .await
        .unwrap();
        for (key, value) in rows {
            store
                .put_state(StateEntry {
                    kind: fauna_protocol::merge_policy::KIND_SHARE_ENDPOINTS.to_string(),
                    key: key.clone(),
                    scope: ACCOUNT_STATE_SCOPE.to_string(),
                    value: canonical_encode(value).unwrap(),
                    merge_meta: None,
                    entry_version: 0,
                    tombstone: false,
                })
                .await
                .unwrap();
        }
        store
    }

    fn share_row(
        set: [u8; 32],
        member: [u8; 32],
        node_id: [u8; 32],
    ) -> (String, fauna_core::share_endpoints::ShareEndpoints) {
        (
            fauna_core::share_endpoints::share_entry_key(&set, &member),
            fauna_core::share_endpoints::ShareEndpoints {
                channel_id: set.to_vec(),
                member_actor: member.to_vec(),
                endpoints: endpoints(node_id),
            },
        )
    }

    /// The happy path: a member's cached advertisement becomes a dial
    /// target under exactly the hygiene a sibling's row gets — the loopback
    /// candidate dropped (PT-4), the LAN one kept on a shared /24, WAN and
    /// relay carried through.
    #[tokio::test]
    async fn a_cached_member_advertisement_becomes_a_dial_target() {
        let store = store_with_share_rows(&[share_row(SET, MEMBER, MEMBER)]).await;
        let ours = ["192.168.1.7".parse().unwrap()];

        let targets = share_dial_targets(&store, &SET, &ours).await.unwrap();

        assert_eq!(targets.len(), 1);
        let t = &targets[0];
        assert_eq!(t.node_id, MEMBER, "the share leg dials the ACTOR key");
        assert_eq!(
            t.candidates.lan_endpoints,
            vec!["192.168.1.9:4433".parse::<SocketAddr>().unwrap()],
            "the loopback candidate must be dropped (PT-4), the LAN one kept"
        );
        assert_eq!(
            t.candidates.wan_endpoint,
            Some("203.0.113.9:4433".parse().unwrap())
        );
        assert!(t.candidates.relay_available);
    }

    /// Dial lists never widen across sets: a member of ANOTHER set is not
    /// this set's counterparty, and admission is per set.
    #[tokio::test]
    async fn rows_of_another_set_are_not_this_sets_dial_targets() {
        let store = store_with_share_rows(&[
            share_row(SET, MEMBER, MEMBER),
            share_row(OTHER_SET, [0x4C; 32], [0x4C; 32]),
        ])
        .await;

        let targets = share_dial_targets(&store, &SET, &[]).await.unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].node_id, MEMBER);
    }

    /// A row whose advertised NodeId is not the member its key names is
    /// skipped — the tampered-row refusal `sibling_dial_targets` runs, for
    /// the same reason: a stored row must never redirect a consumer's dials
    /// at a node the named advertiser does not control.
    #[tokio::test]
    async fn a_row_naming_a_third_node_is_skipped() {
        let store = store_with_share_rows(&[share_row(SET, MEMBER, [0x99; 32])]).await;
        let targets = share_dial_targets(&store, &SET, &[]).await.unwrap();
        assert!(targets.is_empty());
    }

    /// The same refusal on the other redundant half — a row filed under one
    /// set but claiming another.
    #[tokio::test]
    async fn a_row_claiming_a_different_set_than_its_key_is_skipped() {
        let (key, mut value) = share_row(SET, MEMBER, MEMBER);
        value.channel_id = OTHER_SET.to_vec();
        let store = store_with_share_rows(&[(key, value)]).await;
        let targets = share_dial_targets(&store, &SET, &[]).await.unwrap();
        assert!(targets.is_empty());
    }

    /// The rule-7 gate reads the capability list exactly as every other
    /// capability consumer does.
    #[test]
    fn peer_sync_enabled_follows_the_capability_token() {
        assert!(peer_sync_enabled(&["peer-sync".to_string()]));
        assert!(!peer_sync_enabled(&["mail".to_string()]));
        assert!(!peer_sync_enabled(&[]));
    }
}
