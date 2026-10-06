//! Binds this crate's VPS provider CENSUS to its delete/PTR conformance
//! coverage.
//!
//! `VpsDispatch`'s own trait-impl matches (`dispatch.rs`) already force an
//! arm per new [`fauna_provisioning::vps::VpsProvider`] — that is the ONE
//! spelling of the six-provider set that is compile-bound. The other three
//! — the "All six" doc-comment counts in `vps/mod.rs`, the convention that
//! every `delete_server` funnels through `finish_delete`, and this crate's
//! conformance coverage (`vps_delete_server_conformance.rs`,
//! `vps_set_ptr_conformance.rs`, `bundled_conformance.rs`) — are all
//! hand-maintained and could silently miss a new provider. The doc-comment
//! counts already rotted once.
//!
//! This file adds a fourth binding. Both tests below iterate a `match` over
//! `&VpsDispatch` with NO wildcard arm — Rust checks a match's exhaustiveness
//! against the scrutinee's static type, so this fails to COMPILE the moment
//! `VpsDispatch` grows a variant, independent of whether the loop's
//! `prototypes` list was also extended. Request-SHAPE precision
//! (path/method/auth per provider) stays the job of the three files named
//! above; this file's job is coverage-by-construction — a seventh provider
//! cannot reach `main` without a session opening this exact file.

use fauna_provisioning::dispatch::VpsDispatch;
use fauna_provisioning::vps::bundled::BundledVps;
use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::hetzner::Hetzner;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::ovh::Ovh;
use fauna_provisioning::vps::vultr::Vultr;
use fauna_provisioning::vps::{VpsInstance, VpsProvider};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn instance(server_id: &str, ipv4: &str) -> VpsInstance {
    VpsInstance {
        server_id: server_id.into(),
        ipv4: ipv4.into(),
    }
}

/// One dummy-credentialed instance per `VpsDispatch` variant. Never dialed
/// as constructed here — each test below rebuilds against a fresh mock's
/// `base_url` via its own exhaustive match before making a real call.
fn prototypes() -> Vec<VpsDispatch> {
    vec![
        VpsDispatch::Hetzner(Hetzner::new(String::new())),
        VpsDispatch::Digitalocean(DigitalOcean::new(String::new())),
        VpsDispatch::Vultr(Vultr::new(String::new())),
        VpsDispatch::Ovh(Ovh::new(String::new(), String::new(), String::new())),
        VpsDispatch::Linode(Linode::new(String::new())),
        VpsDispatch::Bundled(BundledVps::new(String::new(), String::new())),
    ]
}

/// OVH signs every request against a server-supplied timestamp fetched
/// first (`ovh.rs::get_timestamp`); mounting this on every mock is a no-op
/// for the other five providers, which never call it.
async fn mount_ovh_time(mock: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(mock)
        .await;
}

#[tokio::test]
async fn every_vps_dispatch_variant_delete_is_idempotent_on_404() {
    for prototype in &prototypes() {
        let mock = MockServer::start().await;
        mount_ovh_time(&mock).await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;

        // EXHAUSTIVE — no wildcard arm. `VpsDispatch` growing a variant
        // fails this match at compile time regardless of `prototypes()`.
        let dispatch = match prototype {
            VpsDispatch::Hetzner(_) => {
                VpsDispatch::Hetzner(Hetzner::with_base_url(String::new(), mock.uri()))
            }
            VpsDispatch::Digitalocean(_) => {
                VpsDispatch::Digitalocean(DigitalOcean::with_base_url(String::new(), mock.uri()))
            }
            VpsDispatch::Vultr(_) => {
                VpsDispatch::Vultr(Vultr::with_base_url(String::new(), mock.uri()))
            }
            VpsDispatch::Ovh(_) => VpsDispatch::Ovh(Ovh::with_base_url(
                String::new(),
                String::new(),
                String::new(),
                mock.uri(),
            )),
            VpsDispatch::Linode(_) => {
                VpsDispatch::Linode(Linode::with_base_url(String::new(), mock.uri()))
            }
            VpsDispatch::Bundled(_) => {
                VpsDispatch::Bundled(BundledVps::new(mock.uri(), String::new()))
            }
        };

        let result = dispatch
            .delete_server(&reqwest::Client::new(), &instance("x", "1.2.3.4"))
            .await;
        assert!(
            result.is_ok(),
            "{}: 404 must be a no-op success, got {result:?}",
            dispatch.variant_name()
        );
    }
}

#[tokio::test]
async fn every_vps_dispatch_variant_set_ptr_is_idempotent_on_retry() {
    for prototype in &prototypes() {
        let mock = MockServer::start().await;
        // Fully permissive: the property under test is "calling set_ptr
        // twice with the same fqdn is safe" (the trait doc comment on
        // `VpsProvider::set_ptr`), not any one provider's exact
        // method/path — those stay pinned in `vps_set_ptr_conformance.rs`
        // and `bundled_conformance.rs`. Body is a bare number so OVH's
        // timestamp fetch (routed through this same catch-all) decodes —
        // no provider's `set_ptr` parses a response body at all, so this
        // is otherwise inert.
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
            .mount(&mock)
            .await;

        // EXHAUSTIVE — no wildcard arm; same tripwire as the test above.
        let dispatch = match prototype {
            VpsDispatch::Hetzner(_) => {
                VpsDispatch::Hetzner(Hetzner::with_base_url(String::new(), mock.uri()))
            }
            VpsDispatch::Digitalocean(_) => {
                VpsDispatch::Digitalocean(DigitalOcean::with_base_url(String::new(), mock.uri()))
            }
            VpsDispatch::Vultr(_) => {
                VpsDispatch::Vultr(Vultr::with_base_url(String::new(), mock.uri()))
            }
            VpsDispatch::Ovh(_) => VpsDispatch::Ovh(Ovh::with_base_url(
                String::new(),
                String::new(),
                String::new(),
                mock.uri(),
            )),
            VpsDispatch::Linode(_) => {
                VpsDispatch::Linode(Linode::with_base_url(String::new(), mock.uri()))
            }
            VpsDispatch::Bundled(_) => {
                VpsDispatch::Bundled(BundledVps::new(mock.uri(), String::new()))
            }
        };

        let client = reqwest::Client::new();
        let target = instance("x", "1.2.3.4");
        let first = dispatch.set_ptr(&client, &target, "nest.example.com").await;
        let second = dispatch.set_ptr(&client, &target, "nest.example.com").await;
        assert!(
            first.is_ok() && second.is_ok(),
            "{}: set_ptr called twice with the same fqdn must both succeed, got {first:?} then {second:?}",
            dispatch.variant_name()
        );
    }
}
