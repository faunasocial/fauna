//! The device-endpoint entry's value shape — the peer leg's registry row
//! (`fauna.state.device-endpoints`, registered in
//! `fauna_protocol::merge_policy`; W2.5 (account-data-plane.md § Workstreams) item 3).
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § The peer leg →
//! *Discovery — the replica is its own peer registry* (T5): each replica
//! publishes its device principal's public key (= its iroh NodeId), current
//! LAN + last-known public addresses, and the relay URL its nest advertises,
//! as a class-2 **whole-record LWW** state entry on the account-state scope.
//! The entry's logical key is the publishing device's writer id (hex), so
//! each device owns exactly one row and concurrent writers never collide on
//! a key.
//!
//! # Rung: fleet-only (the W2.5 item-3 ruling)
//!
//! Default-narrow holds — there is no grant-shaped consumer of dial
//! candidates, and endpoint entries are *location data*. The R14 (account-data-plane.md § The ratified decisions) sealing gate
//! therefore refuses production origination until the generation schedule
//! exists, and for this kind that is a **feature**: generation keying is
//! exactly what severs a removed (possibly stolen) device from reading the
//! fleet's future addresses. See the registration row and the charter's
//! § The audience ladder → *The device-endpoints rung*.
//!
//! # Evolution posture (contrast `crate::seen_set`)
//!
//! An LWW kind never re-encodes a merged output — a reader adopts or keeps
//! verbatim bytes — so tolerant decoding cannot strip a newer writer's
//! fields. This type therefore accepts unknown fields and defaults missing
//! ones: additive evolution is in-place, no sibling kind needed.

use serde::{Deserialize, Serialize};

/// One device's dial candidates — the value of one
/// `fauna.state.device-endpoints` entry.
///
/// Addresses are socket-address strings (`"192.168.1.7:4433"`,
/// `"[2001:db8::1]:4433"`); the discovery consumer parses them and applies
/// the shipped LAN arithmetic (`fauna_wireguard::endpoint` — RFC-1918 +
/// shared /24) to the LAN list. Staleness is judged by the entry's LWW stamp
/// (`fauna_protocol::merge_policy::LwwStamp`), not by any field here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEndpoints {
    /// The publishing device principal's public key — its transport NodeId.
    /// Redundant with the entry's logical key (the writer id hex) on
    /// purpose: consumers dial from the decoded value without re-parsing
    /// key strings, and the walk can cross-check the two.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub node_id: [u8; 32],
    /// Current LAN socket addresses, most-preferred first.
    #[serde(default)]
    pub lan_addrs: Vec<String>,
    /// Last-known public (reflexive) socket addresses, most-preferred first.
    #[serde(default)]
    pub public_addrs: Vec<String>,
    /// The relay URL this device's nest advertises
    /// (`NestInfoReply.iroh_relay_url`), when one exists.
    #[serde(default)]
    pub relay_url: Option<String>,
}

/// The **plane-published** form of one `fauna.state.device-endpoints` entry:
/// [`DeviceEndpoints`] plus the facts only the fleet may read.
///
/// A superset rather than a field on [`DeviceEndpoints`], because that type is
/// also *carried* — on the admit exchange, the custody ceremony, the share
/// plane's advertisements — to peers outside the fleet, and
/// [`Self::enrolled_row`] is fleet-only. The evolution posture above is what
/// makes the split free: a reader decoding this entry as [`DeviceEndpoints`]
/// ignores the extra field, and an entry with no row stated encodes
/// byte-identically to the [`DeviceEndpoints`] every earlier build published.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEndpointsEntry {
    /// See [`DeviceEndpoints::node_id`].
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub node_id: [u8; 32],
    /// See [`DeviceEndpoints::lan_addrs`].
    #[serde(default)]
    pub lan_addrs: Vec<String>,
    /// See [`DeviceEndpoints::public_addrs`].
    #[serde(default)]
    pub public_addrs: Vec<String>,
    /// See [`DeviceEndpoints::relay_url`].
    #[serde(default)]
    pub relay_url: Option<String>,
    /// The nest `sync_devices` row (`device_id` hex) this device **enrolled
    /// on** — the registration latch's row half, stated by the device itself.
    /// It is what binds a devices-page row to a fleet member from client-held
    /// truth: the nest cannot seal this kind, so it cannot re-pair a row with
    /// another member's principal ([`crate::fleet_removal`]). `None` until the
    /// enrollment's nest legs have first succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled_row: Option<String>,
}

impl DeviceEndpointsEntry {
    /// The entry publishing `endpoints` with this device's row statement.
    #[must_use]
    pub fn new(endpoints: DeviceEndpoints, enrolled_row: Option<String>) -> Self {
        Self {
            node_id: endpoints.node_id,
            lan_addrs: endpoints.lan_addrs,
            public_addrs: endpoints.public_addrs,
            relay_url: endpoints.relay_url,
            enrolled_row,
        }
    }
}

/// LAN dial candidates from where a listener bound: a specifically-bound
/// non-loopback address rides as-is; an unspecified IPv4 bind contributes its
/// port, crossed with the device's interface addresses. Shared by the
/// same-account peer leg's published facts, the share plane's
/// advertisements, and the co-present ceremony's carried candidates, so no
/// two legs can compose their candidates differently.
///
/// An unspecified IPv6 bind (`[::]`) contributes nothing: the interface list
/// is IPv4, and whether a `[::]` socket also answers IPv4 is per-OS (Windows
/// defaults to v6-only). iroh binds one beside the `0.0.0.0` socket on its own
/// port, so crossing it would advertise a port nothing IPv4 listens on
/// (`p2p.md` § Offline share initiation, contract point 1).
pub fn lan_socket_addr_candidates(
    bound: &[std::net::SocketAddr],
    lan_ips: &[std::net::Ipv4Addr],
) -> Vec<std::net::SocketAddr> {
    let mut lan_addrs: Vec<std::net::SocketAddr> = Vec::new();
    let mut ports: Vec<u16> = Vec::new();
    for addr in bound {
        if addr.ip().is_unspecified() {
            if addr.is_ipv4() && !ports.contains(&addr.port()) {
                ports.push(addr.port());
            }
        } else if !addr.ip().is_loopback() && !lan_addrs.contains(addr) {
            lan_addrs.push(*addr);
        }
    }
    for port in ports {
        for ip in lan_ips {
            let a = std::net::SocketAddr::new(std::net::IpAddr::V4(*ip), port);
            if !lan_addrs.contains(&a) {
                lan_addrs.push(a);
            }
        }
    }
    lan_addrs
}

/// [`lan_socket_addr_candidates`] in the string spelling
/// [`DeviceEndpoints::lan_addrs`] stores — the wire/at-rest edge; consumers
/// that dial keep the typed form.
pub fn lan_socket_addrs(
    bound: &[std::net::SocketAddr],
    lan_ips: &[std::net::Ipv4Addr],
) -> Vec<String> {
    lan_socket_addr_candidates(bound, lan_ips)
        .iter()
        .map(|a| a.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DeviceEndpoints {
        DeviceEndpoints {
            node_id: [7u8; 32],
            lan_addrs: vec!["192.168.1.7:4433".into()],
            public_addrs: vec!["203.0.113.9:4433".into()],
            relay_url: Some("https://relay.example/".into()),
        }
    }

    #[test]
    fn canonical_round_trip() {
        let v = sample();
        let bytes = crate::encoding::canonical_encode(&v).unwrap();
        let back: DeviceEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, v);
    }

    /// The evolution posture, pinned: an LWW value from a newer build — same
    /// fields plus one this build never heard of — still decodes (the unknown
    /// field is ignored), because an LWW reader never re-encodes a merged
    /// output, so nothing is stripped from anyone's bytes.
    #[test]
    fn a_newer_builds_value_decodes_tolerantly() {
        #[derive(Serialize)]
        struct V2 {
            #[serde(with = "serde_bytes")]
            node_id: [u8; 32],
            lan_addrs: Vec<String>,
            public_addrs: Vec<String>,
            relay_url: Option<String>,
            transport_hint: String, // the field this build predates
        }
        let bytes = crate::encoding::canonical_encode(&V2 {
            node_id: [7u8; 32],
            lan_addrs: vec!["192.168.1.7:4433".into()],
            public_addrs: Vec::new(),
            relay_url: None,
            transport_hint: "quic-v2".into(),
        })
        .unwrap();
        let got: DeviceEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(got.node_id, [7u8; 32]);
        assert_eq!(got.lan_addrs, vec!["192.168.1.7:4433".to_string()]);
    }

    /// The candidate composition: a concrete bind rides as-is, an
    /// unspecified IPv4 bind crosses its port with every interface address,
    /// loopback never advertises, and duplicates collapse.
    /// The split's two compat claims: an entry with no row stated is
    /// byte-identical to what every earlier build published, and an entry
    /// that states one still decodes as the carried [`DeviceEndpoints`].
    #[test]
    fn the_plane_entry_is_a_tolerant_superset_of_the_carried_value() {
        let v = sample();
        let plain = crate::encoding::canonical_encode(&v).unwrap();
        let unbound =
            crate::encoding::canonical_encode(&DeviceEndpointsEntry::new(v.clone(), None)).unwrap();
        assert_eq!(unbound, plain, "no row stated — the pre-binding bytes");

        let bound = crate::encoding::canonical_encode(&DeviceEndpointsEntry::new(
            v.clone(),
            Some("ab".repeat(32)),
        ))
        .unwrap();
        let carried: DeviceEndpoints = crate::encoding::canonical_decode(&bound).unwrap();
        assert_eq!(carried, v);
        let entry: DeviceEndpointsEntry = crate::encoding::canonical_decode(&bound).unwrap();
        assert_eq!(entry.enrolled_row, Some("ab".repeat(32)));
        // And the older shape reads as an entry with no statement.
        let old: DeviceEndpointsEntry = crate::encoding::canonical_decode(&plain).unwrap();
        assert_eq!(old.enrolled_row, None);
    }

    #[test]
    fn lan_socket_addrs_crosses_unspecified_binds_and_drops_loopback() {
        let bound: Vec<std::net::SocketAddr> = vec![
            "0.0.0.0:4711".parse().unwrap(),
            "192.168.1.20:4711".parse().unwrap(),
            "127.0.0.1:4711".parse().unwrap(),
        ];
        let ips: Vec<std::net::Ipv4Addr> =
            vec!["192.168.1.20".parse().unwrap(), "10.0.0.5".parse().unwrap()];
        assert_eq!(
            lan_socket_addrs(&bound, &ips),
            vec!["192.168.1.20:4711".to_string(), "10.0.0.5:4711".to_string()]
        );
        assert!(lan_socket_addrs(&[], &ips).is_empty(), "no bind, no ports");
    }

    /// An IPv6 socket's port never pairs with an IPv4 address. iroh binds a
    /// `[::]` socket beside the `0.0.0.0` one, on its own port; where that
    /// socket is v6-only (the Windows default) `10.x:<its port>` has no
    /// listener, and every send to it draws a port-unreachable back.
    #[test]
    fn an_ipv6_sockets_port_never_rides_an_ipv4_candidate() {
        let bound: Vec<std::net::SocketAddr> = vec![
            "0.0.0.0:4711".parse().unwrap(),
            "[::]:4712".parse().unwrap(),
        ];
        let ips: Vec<std::net::Ipv4Addr> = vec!["192.168.1.20".parse().unwrap()];
        assert_eq!(
            lan_socket_addrs(&bound, &ips),
            vec!["192.168.1.20:4711".to_string()]
        );
        let v6_only: Vec<std::net::SocketAddr> = vec!["[::]:4712".parse().unwrap()];
        assert!(lan_socket_addrs(&v6_only, &ips).is_empty());
    }
}
