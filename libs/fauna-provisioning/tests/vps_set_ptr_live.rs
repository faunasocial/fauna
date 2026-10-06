//! Live PTR API smoke tests — exercise each VPS provider's real reverse-DNS
//! endpoint end-to-end.
//!
//! These tests catch upstream API drift that the wiremock-based conformance
//! tests can't see (e.g. a provider renaming a JSON field, changing an HTTP
//! method, or adding a required header). They are NOT run by default and have
//! no `#[ignore]` attribute either — they self-skip via env-var check, which
//! matches the pattern used by `porkbun_registrar.rs` (the only other live
//! integration test in this crate).
//!
//! ## Required env vars (per provider)
//!
//! Each test reads its provider's credentials AND a long-lived
//! `*_TEST_SERVER_ID` / `*_TEST_IPV4` / `*_TEST_FQDN` triple. The test calls
//! `set_ptr` on the existing instance with the FQDN and asserts success.
//!
//! - `*_TEST_FQDN` MUST forward-resolve (A record) to `*_TEST_IPV4` — Hetzner
//!   and Linode reject PTR updates whose target doesn't forward-resolve to the
//!   IP. Configure the A record at your DNS host once and reuse the test
//!   instance across runs.
//! - `*_TEST_SERVER_ID` is the provider's instance identifier (Hetzner/DO/Vultr
//!   all use a stable ID; Linode and OVH use the IPv4-keyed PTR endpoint, so
//!   the server ID is informational only).
//!
//! Per-provider env vars:
//!
//! | Provider     | Credentials                                    | Test instance                                     |
//! |--------------|------------------------------------------------|---------------------------------------------------|
//! | Hetzner      | `HETZNER_API_TOKEN`                            | `HETZNER_TEST_SERVER_ID`, `HETZNER_TEST_IPV4`, `HETZNER_TEST_FQDN` |
//! | DigitalOcean | `DIGITALOCEAN_API_TOKEN`                       | `DIGITALOCEAN_TEST_SERVER_ID`, `DIGITALOCEAN_TEST_IPV4`, `DIGITALOCEAN_TEST_FQDN` |
//! | Vultr        | `VULTR_API_KEY`                                | `VULTR_TEST_SERVER_ID`, `VULTR_TEST_IPV4`, `VULTR_TEST_FQDN` |
//! | Linode       | `LINODE_API_TOKEN`                             | `LINODE_TEST_SERVER_ID`, `LINODE_TEST_IPV4`, `LINODE_TEST_FQDN` |
//! | OVH          | `OVH_APP_KEY`, `OVH_APP_SECRET`, `OVH_CONSUMER_KEY` | `OVH_TEST_SERVER_ID`, `OVH_TEST_IPV4`, `OVH_TEST_FQDN` |
//!
//! ## Running
//!
//! Set the env vars for whichever providers you want to smoke-test, then:
//!
//! ```bash
//! cargo test -p fauna-provisioning --test vps_set_ptr_live -- --nocapture
//! ```
//!
//! Each provider's test runs independently. Missing env vars => the test
//! prints a "skipping" line and returns Ok.

use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::hetzner::Hetzner;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::ovh::Ovh;
use fauna_provisioning::vps::vultr::Vultr;
use fauna_provisioning::vps::{VpsInstance, VpsProvider};

/// Read a test instance triple `(server_id, ipv4, fqdn)` from env vars,
/// returning None if any are missing.
fn instance_from_env(prefix: &str) -> Option<(VpsInstance, String)> {
    let server_id = std::env::var(format!("{prefix}_TEST_SERVER_ID")).ok()?;
    let ipv4 = std::env::var(format!("{prefix}_TEST_IPV4")).ok()?;
    let fqdn = std::env::var(format!("{prefix}_TEST_FQDN")).ok()?;
    Some((VpsInstance { server_id, ipv4 }, fqdn))
}

#[tokio::test]
async fn hetzner_set_ptr_live() {
    let Ok(token) = std::env::var("HETZNER_API_TOKEN") else {
        eprintln!("skipping: HETZNER_API_TOKEN not set");
        return;
    };
    let Some((instance, fqdn)) = instance_from_env("HETZNER") else {
        eprintln!("skipping: HETZNER_TEST_SERVER_ID/IPV4/FQDN not all set");
        return;
    };
    let hetzner = Hetzner::new(token);
    let client = reqwest::Client::new();
    hetzner
        .set_ptr(&client, &instance, &fqdn)
        .await
        .expect("Hetzner set_ptr against real API");
}

#[tokio::test]
async fn digitalocean_set_ptr_live() {
    let Ok(token) = std::env::var("DIGITALOCEAN_API_TOKEN") else {
        eprintln!("skipping: DIGITALOCEAN_API_TOKEN not set");
        return;
    };
    let Some((instance, fqdn)) = instance_from_env("DIGITALOCEAN") else {
        eprintln!("skipping: DIGITALOCEAN_TEST_SERVER_ID/IPV4/FQDN not all set");
        return;
    };
    let dop = DigitalOcean::new(token);
    let client = reqwest::Client::new();
    dop.set_ptr(&client, &instance, &fqdn)
        .await
        .expect("DigitalOcean set_ptr against real API");
}

#[tokio::test]
async fn vultr_set_ptr_live() {
    let Ok(token) = std::env::var("VULTR_API_KEY") else {
        eprintln!("skipping: VULTR_API_KEY not set");
        return;
    };
    let Some((instance, fqdn)) = instance_from_env("VULTR") else {
        eprintln!("skipping: VULTR_TEST_SERVER_ID/IPV4/FQDN not all set");
        return;
    };
    let vultr = Vultr::new(token);
    let client = reqwest::Client::new();
    vultr
        .set_ptr(&client, &instance, &fqdn)
        .await
        .expect("Vultr set_ptr against real API");
}

#[tokio::test]
async fn linode_set_ptr_live() {
    let Ok(token) = std::env::var("LINODE_API_TOKEN") else {
        eprintln!("skipping: LINODE_API_TOKEN not set");
        return;
    };
    let Some((instance, fqdn)) = instance_from_env("LINODE") else {
        eprintln!("skipping: LINODE_TEST_SERVER_ID/IPV4/FQDN not all set");
        return;
    };
    let linode = Linode::new(token);
    let client = reqwest::Client::new();
    linode
        .set_ptr(&client, &instance, &fqdn)
        .await
        .expect("Linode set_ptr against real API");
}

#[tokio::test]
async fn ovh_set_ptr_live() {
    let Ok(app_key) = std::env::var("OVH_APP_KEY") else {
        eprintln!("skipping: OVH_APP_KEY not set");
        return;
    };
    let Ok(app_secret) = std::env::var("OVH_APP_SECRET") else {
        eprintln!("skipping: OVH_APP_SECRET not set");
        return;
    };
    let Ok(consumer_key) = std::env::var("OVH_CONSUMER_KEY") else {
        eprintln!("skipping: OVH_CONSUMER_KEY not set");
        return;
    };
    let Some((instance, fqdn)) = instance_from_env("OVH") else {
        eprintln!("skipping: OVH_TEST_SERVER_ID/IPV4/FQDN not all set");
        return;
    };
    let ovh = Ovh::new(app_key, app_secret, consumer_key);
    let client = reqwest::Client::new();
    ovh.set_ptr(&client, &instance, &fqdn)
        .await
        .expect("OVH set_ptr against real API");
}
