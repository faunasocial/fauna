//! LAN interface enumeration and same-network heuristic.
//!
//! Pure RFC-1918 + shared-subnet arithmetic over local interface
//! addresses — no tunnel, no transport, no wire. It lived in
//! `fauna-wireguard` until that stack was deleted (iroh is the only
//! substrate); [`discovery`](crate::discovery) is its one consumer, for
//! both halves of the peer leg: composing this device's own
//! `EndpointFacts.lan_addrs` and filtering a sibling's cached candidates.

use std::net::Ipv4Addr;

/// Discover all private-range IPv4 addresses on local network interfaces.
pub fn discover_lan_candidates() -> Vec<Ipv4Addr> {
    let mut candidates = Vec::new();
    if let Ok(addrs) = if_addrs::get_if_addrs() {
        for iface in addrs {
            if iface.is_loopback() {
                continue;
            }
            if let std::net::IpAddr::V4(v4) = iface.ip()
                && is_private_ip(v4)
            {
                candidates.push(v4);
            }
        }
    }
    candidates
}

/// Format LAN candidates as "ip:port" strings for the peer API.
pub fn lan_endpoints_with_port(candidates: &[Ipv4Addr], port: u16) -> Vec<String> {
    candidates.iter().map(|ip| format!("{ip}:{port}")).collect()
}

/// Check if an IPv4 address is in a private range (RFC 1918 + link-local).
pub fn is_private_ip(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    matches!(
        octets,
        [10, ..] | [172, 16..=31, ..] | [192, 168, ..] | [169, 254, ..]
    )
}

/// Check if two IPs are in the same subnet.
pub fn is_same_subnet(a: Ipv4Addr, b: Ipv4Addr, prefix_len: u8) -> bool {
    if prefix_len == 0 {
        return true;
    }
    let mask = !0u32 << (32 - prefix_len);
    (u32::from(a) & mask) == (u32::from(b) & mask)
}

/// Determine if a LAN probe should be attempted.
/// Returns true if any of our LAN IPs shares a /24 subnet with any peer LAN endpoint.
pub fn should_attempt_lan_probe(our_ips: &[Ipv4Addr], peer_lan_endpoints: &[Ipv4Addr]) -> bool {
    for our_ip in our_ips {
        for peer_ip in peer_lan_endpoints {
            if is_same_subnet(*our_ip, *peer_ip, 24) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_lan_candidates_returns_private_ips() {
        let candidates = discover_lan_candidates();
        for ip in &candidates {
            assert!(is_private_ip(*ip), "expected private IP, got {ip}");
        }
    }

    #[test]
    fn is_private_ip_classification() {
        assert!(is_private_ip(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_private_ip(Ipv4Addr::new(172, 16, 5, 1)));
        assert!(is_private_ip(Ipv4Addr::new(192, 168, 1, 42)));
        assert!(!is_private_ip(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_private_ip(Ipv4Addr::new(203, 0, 113, 1)));
    }

    #[test]
    fn same_subnet_check() {
        assert!(is_same_subnet(
            Ipv4Addr::new(192, 168, 1, 42),
            Ipv4Addr::new(192, 168, 1, 100),
            24
        ));
        assert!(!is_same_subnet(
            Ipv4Addr::new(192, 168, 1, 42),
            Ipv4Addr::new(192, 168, 2, 100),
            24
        ));
        assert!(is_same_subnet(
            Ipv4Addr::new(10, 0, 0, 5),
            Ipv4Addr::new(10, 0, 0, 1),
            24
        ));
    }

    #[test]
    fn should_attempt_lan_probe_logic() {
        let our = vec![Ipv4Addr::new(192, 168, 1, 42)];
        let peer = vec![Ipv4Addr::new(192, 168, 1, 100)];
        assert!(should_attempt_lan_probe(&our, &peer));

        let peer_diff = vec![Ipv4Addr::new(10, 0, 0, 1)];
        assert!(!should_attempt_lan_probe(&our, &peer_diff));
    }
}
