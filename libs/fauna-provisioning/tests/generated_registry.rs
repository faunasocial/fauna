use fauna_provisioning::{Capability, PROVIDERS, ProviderId};

#[test]
fn three_reference_providers_present() {
    let ids: Vec<_> = PROVIDERS.iter().map(|p| p.id).collect();
    assert!(ids.contains(&ProviderId::Cloudflare));
    assert!(ids.contains(&ProviderId::Porkbun));
    assert!(ids.contains(&ProviderId::Hetzner));
}

#[test]
fn cloudflare_is_dns_and_registrar() {
    let cf = PROVIDERS
        .iter()
        .find(|p| p.id == ProviderId::Cloudflare)
        .unwrap();
    assert!(cf.capabilities.contains(&Capability::Dns));
    assert!(cf.capabilities.contains(&Capability::Registrar));
}

#[test]
fn porkbun_is_dns_and_registrar() {
    let pb = PROVIDERS
        .iter()
        .find(|p| p.id == ProviderId::Porkbun)
        .unwrap();
    assert!(pb.capabilities.contains(&Capability::Dns));
    assert!(pb.capabilities.contains(&Capability::Registrar));
}
