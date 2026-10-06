use fauna_peer_sync::lan::*;
use std::net::Ipv4Addr;

#[test]
fn discover_candidates_on_this_machine() {
    let candidates = discover_lan_candidates();
    println!(
        "discovered {} LAN candidates: {:?}",
        candidates.len(),
        candidates
    );
    for ip in &candidates {
        assert!(is_private_ip(*ip));
    }
}

#[test]
fn lan_endpoints_with_port_formatting() {
    let ips = vec![Ipv4Addr::new(192, 168, 1, 42), Ipv4Addr::new(10, 0, 0, 5)];
    let eps = lan_endpoints_with_port(&ips, 51820);
    assert_eq!(eps, vec!["192.168.1.42:51820", "10.0.0.5:51820"]);
}

#[test]
fn same_network_heuristic_works() {
    let our = vec![Ipv4Addr::new(192, 168, 1, 42)];
    let peer = vec![Ipv4Addr::new(192, 168, 1, 100)];
    assert!(should_attempt_lan_probe(&our, &peer));

    let peer_diff = vec![Ipv4Addr::new(10, 0, 0, 1)];
    assert!(!should_attempt_lan_probe(&our, &peer_diff));

    assert!(!should_attempt_lan_probe(&our, &[]));

    let our_multi = vec![Ipv4Addr::new(192, 168, 1, 42), Ipv4Addr::new(10, 0, 0, 5)];
    let peer_ten = vec![Ipv4Addr::new(10, 0, 0, 1)];
    assert!(should_attempt_lan_probe(&our_multi, &peer_ten));
}
