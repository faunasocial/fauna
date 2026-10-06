use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::ProvisionError;
use crate::namecheap_api::{NamecheapApi, attr, elements};
use crate::proxy::BuildEnv;

/// Namecheap DNS provider.
///
/// The transport — global parameters, the HTTP-200-with-`Status="ERROR"`
/// envelope, and the IP-allowlist self-heal — lives in
/// [`crate::namecheap_api`], shared with
/// [`crate::registrar::namecheap::NamecheapRegistrar`]: Namecheap serves every
/// capability from one endpoint, so the two adapters are two views of a single
/// authenticated connection.
pub struct Namecheap {
    api: NamecheapApi,
}

/// Split a domain like "example.com" into ("example", "com"),
/// or "example.co.uk" into ("example", "co.uk").
fn split_domain(domain: &str) -> Result<(String, String), ProvisionError> {
    let parts: Vec<&str> = domain.splitn(2, '.').collect();
    if parts.len() != 2 {
        return Err(ProvisionError::Parse(format!("Invalid domain: {domain}")));
    }
    if parts[0].is_empty() || parts[1].is_empty() {
        return Err(ProvisionError::Parse(format!("Invalid domain: {domain}")));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// A parsed host record from Namecheap getHosts response.
struct ExistingHost {
    name: String,
    record_type: String,
    value: String,
    ttl: String,
    mx_pref: String,
}

/// Parse existing host records from Namecheap getHosts XML response.
/// Looks for `<host .../>` elements with Name, Type, Address, TTL, MXPref attributes.
fn parse_hosts_from_xml(xml: &str) -> Vec<ExistingHost> {
    elements(xml, "host")
        .into_iter()
        .map(|el| {
            let get = |name: &str| attr(el, name).unwrap_or_default();
            ExistingHost {
                name: get("Name"),
                record_type: get("Type"),
                value: get("Address"),
                ttl: get("TTL"),
                mx_pref: get("MXPref"),
            }
        })
        .collect()
}

/// Split a long TXT value into ≤255-byte chunks. RFC 1035 caps each TXT
/// character-string at 255 bytes; Namecheap's `Address{n}` field on
/// `setHosts` enforces this limit. Long values (DKIM, ~400+ chars) must
/// span multiple host entries with the same `HostName{n}`. Returns one
/// chunk per resulting host entry in submission order.
fn split_long_txt(value: &str) -> Vec<String> {
    if value.is_empty() {
        return vec![value.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = value;
    while !remaining.is_empty() {
        // DKIM keys are ASCII so the boundary is always safe; this backs off
        // defensively if a multi-byte sequence ever lands here.
        let chunk = fauna_core::encoding::truncate_to_char_boundary(remaining, 255);
        chunks.push(chunk.to_string());
        remaining = &remaining[chunk.len()..];
    }
    chunks
}

/// Parse domain names from Namecheap `domains.getList` XML response.
fn parse_domains_from_xml(xml: &str) -> Vec<String> {
    elements(xml, "Domain")
        .into_iter()
        .filter_map(|el| attr(el, "Name"))
        .collect()
}

impl Namecheap {
    pub fn new(api_user: String, api_key: String) -> Self {
        Self {
            api: NamecheapApi::new(api_user, api_key),
        }
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing. See `tests/cors_policy_bijection.rs`.
    pub fn new_for_env(api_user: String, api_key: String, env: BuildEnv) -> Self {
        Self {
            api: NamecheapApi::new_for_env(api_user, api_key, env),
        }
    }

    /// The API base this adapter will call.
    pub fn api_base(&self) -> &str {
        self.api.api_base()
    }

    /// Test-only constructor that lets the conformance harness point the
    /// adapter at a wiremock server.
    pub fn with_base_url(api_user: String, api_key: String, api_base: String) -> Self {
        Self {
            api: NamecheapApi::with_base_url(api_user, api_key, api_base),
        }
    }

    /// Override the `ClientIp` the transport claims — see
    /// [`NamecheapApi::set_client_ip`]. Only an optimisation: the self-heal
    /// retry makes correctness independent of it.
    pub fn set_client_ip(&mut self, client_ip: String) {
        self.api.set_client_ip(client_ip);
    }

    /// The `ClientIp` this adapter currently claims.
    pub fn client_ip(&self) -> &str {
        self.api.client_ip()
    }
}

impl DnsProvider for Namecheap {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        let xml = self
            .api
            .call(client, "namecheap.domains.getList", &[])
            .await?;
        let domains = parse_domains_from_xml(&xml);
        Ok(domains
            .into_iter()
            .map(|name| DnsZone {
                id: name.clone(),
                name,
            })
            .collect())
    }

    async fn create_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        record: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        // Namecheap's `setHosts` API has no TLSA `RecordType` (its set is
        // A/AAAA/CNAME/MX/MXE/TXT/URL/URL301/FRAME/NS/SRV/CAA/ALIAS — no TLSA;
        // verified 2026-06-07), so the floor-MX DANE pin can't be published here.
        // Fail loudly with a clear reason rather than posting an unsupported
        // `RecordType` that Namecheap rejects opaquely (or, worse, drops). The
        // managed TLSA reconcile catches this non-fatally — MTA-STS still
        // protects the MX (`tls-certificates.md` § D).
        if record.record_type == "TLSA" {
            return Err(ProvisionError::Other(
                "Namecheap does not support TLSA/DANE records (its setHosts API has no TLSA \
                 record type); the floor-MX DANE pin can't be published on Namecheap — MTA-STS \
                 still protects the MX"
                    .to_string(),
            ));
        }

        let (sld, tld) = split_domain(zone_id)?;

        // Step 1: Fetch existing records to avoid replacing all hosts.
        let xml = self
            .api
            .call(
                client,
                "namecheap.domains.dns.getHosts",
                &[
                    ("SLD".to_string(), sld.clone()),
                    ("TLD".to_string(), tld.clone()),
                ],
            )
            .await?;
        let existing = parse_hosts_from_xml(&xml);

        // Step 2: Build query params with all existing records plus the new one.
        let mut params: Vec<(String, String)> = vec![
            ("SLD".to_string(), sld.clone()),
            ("TLD".to_string(), tld.clone()),
        ];

        // Add existing records.
        for (i, host) in existing.iter().enumerate() {
            let n = i + 1;
            params.push((format!("HostName{n}"), host.name.clone()));
            params.push((format!("RecordType{n}"), host.record_type.clone()));
            params.push((format!("Address{n}"), host.value.clone()));
            params.push((format!("TTL{n}"), host.ttl.clone()));
            if !host.mx_pref.is_empty() {
                params.push((format!("MXPref{n}"), host.mx_pref.clone()));
            }
        }

        // Append the new record. Long TXT values (DKIM is ~400 chars) hit
        // Namecheap's 255-byte `Address{n}` limit, so split into multiple
        // host entries with the same name and sequential indices.
        let new_chunks = if record.record_type == "TXT" && record.value.len() > 255 {
            split_long_txt(&record.value)
        } else {
            vec![record.value.clone()]
        };
        for (n, chunk) in (existing.len() + 1..).zip(&new_chunks) {
            params.push((format!("HostName{n}"), record.name.clone()));
            params.push((format!("RecordType{n}"), record.record_type.clone()));
            params.push((format!("Address{n}"), chunk.clone()));
            params.push((format!("TTL{n}"), record.ttl.to_string()));
            if let Some(priority) = record.priority {
                params.push((format!("MXPref{n}"), priority.to_string()));
            }
        }

        // Step 3: Send all records together. The body is discarded, but it must
        // still be checked — a `Status="ERROR"` here arrives under HTTP 200 and
        // would otherwise report a zone write that never happened.
        self.api
            .call(client, "namecheap.domains.dns.setHosts", &params)
            .await?;

        Ok(())
    }

    async fn find_records(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        let (sld, tld) = split_domain(zone_id)?;
        let xml = self
            .api
            .call(
                client,
                "namecheap.domains.dns.getHosts",
                &[
                    ("SLD".to_string(), sld.clone()),
                    ("TLD".to_string(), tld.clone()),
                ],
            )
            .await?;
        // Namecheap's "host" name is the subdomain ("@" for the apex), which is
        // exactly what `name` already carries — the seam relativizes for every
        // provider leaving `record_names_relative_to_zone` at its `true`
        // default. Matched verbatim: see the contract note on that method.
        Ok(parse_hosts_from_xml(&xml)
            .into_iter()
            .filter(|h| h.name == name && h.record_type == record_type)
            .map(|h| DnsRecord {
                record_type: h.record_type,
                name: h.name,
                value: h.value,
                ttl: h.ttl.parse().unwrap_or(0),
                priority: h.mx_pref.parse().ok(),
            })
            .collect())
    }

    async fn delete_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
        value: &str,
    ) -> Result<(), ProvisionError> {
        let (sld, tld) = split_domain(zone_id)?;

        // Namecheap has no per-record delete: setHosts replaces the entire host
        // set. So read every host, drop the one matching (name, type, value),
        // and write the survivors back. The target value is matched verbatim;
        // `_acme-challenge` (and every other managed) TXT value is well under
        // the 255-byte chunking threshold, so it is never split across host
        // entries on create and matches whole here.
        let xml = self
            .api
            .call(
                client,
                "namecheap.domains.dns.getHosts",
                &[
                    ("SLD".to_string(), sld.clone()),
                    ("TLD".to_string(), tld.clone()),
                ],
            )
            .await?;
        // `name` is the already-relative host name (see `find_records`).
        let existing = parse_hosts_from_xml(&xml);
        let before = existing.len();
        let remaining: Vec<ExistingHost> = existing
            .into_iter()
            .filter(|h| !(h.name == name && h.record_type == record_type && h.value == value))
            .collect();

        // Idempotent: nothing matched, so don't rewrite the zone needlessly.
        if remaining.len() == before {
            return Ok(());
        }

        let mut params: Vec<(String, String)> = vec![
            ("SLD".to_string(), sld.clone()),
            ("TLD".to_string(), tld.clone()),
        ];
        for (i, host) in remaining.iter().enumerate() {
            let n = i + 1;
            params.push((format!("HostName{n}"), host.name.clone()));
            params.push((format!("RecordType{n}"), host.record_type.clone()));
            params.push((format!("Address{n}"), host.value.clone()));
            params.push((format!("TTL{n}"), host.ttl.clone()));
            if !host.mx_pref.is_empty() {
                params.push((format!("MXPref{n}"), host.mx_pref.clone()));
            }
        }

        // As in `create_record`: the body is discarded but must still be
        // checked, or a rejected delete reports as a successful one.
        self.api
            .call(client, "namecheap.domains.dns.setHosts", &params)
            .await?;

        Ok(())
    }

    fn base_url(&self) -> &str {
        self.api.api_base()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_domain() {
        let (sld, tld) = split_domain("example.com").unwrap();
        assert_eq!(sld, "example");
        assert_eq!(tld, "com");
    }

    #[test]
    fn test_split_domain_multi_tld() {
        let (sld, tld) = split_domain("example.co.uk").unwrap();
        assert_eq!(sld, "example");
        assert_eq!(tld, "co.uk");
    }

    #[test]
    fn test_split_domain_invalid() {
        assert!(split_domain("nodot").is_err());
        assert!(split_domain(".nodomain").is_err());
        assert!(split_domain("notld.").is_err());
    }

    #[test]
    fn test_split_long_txt_short() {
        let v = "v=spf1 a mx -all";
        assert_eq!(split_long_txt(v), vec![v.to_string()]);
    }

    #[test]
    fn test_split_long_txt_at_boundary() {
        let v: String = "A".repeat(255);
        assert_eq!(split_long_txt(&v), vec![v.clone()]);
    }

    #[test]
    fn test_split_long_txt_dkim_shaped() {
        let v: String = "A".repeat(400);
        let chunks = split_long_txt(&v);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 255);
        assert_eq!(chunks[1].len(), 145);
        assert_eq!(chunks.concat(), v);
    }

    /// The synthetic 400-char case above only approximates a real DKIM TXT
    /// value — it never traces through `dkim.rs`'s actual keygen output,
    /// which is 410 chars (255 + 155) and, unlike `"A".repeat(400)`, spans
    /// base64's full `+`/`/`/`=` alphabet. Splitting must round-trip it too.
    #[test]
    fn test_split_long_txt_real_rsa_dkim_value() {
        let kp = crate::dkim::generate_rsa_2048().expect("dkim keygen");
        let v = kp.public_dns_value;
        let chunks = split_long_txt(&v);
        assert!(
            chunks.iter().all(|c| c.len() <= 255),
            "every chunk must fit Namecheap's 255-byte Address{{n}} limit: {chunks:?}"
        );
        assert_eq!(chunks.concat(), v, "chunks must reassemble losslessly");
        assert_eq!(
            chunks.len(),
            v.len().div_ceil(255),
            "chunk count must match the real value's actual length, not an assumed one"
        );
    }

    // The transport's own tests — the API-error envelope, the allowlist
    // self-heal, and the XML scanning primitives — live with the transport in
    // `crate::namecheap_api`, shared with the registrar adapter.

    /// Namecheap rejects a missing `ClientIp` outright (1010105), so the
    /// adapter must never construct itself with an empty one.
    #[test]
    fn constructors_seed_a_syntactically_valid_client_ip() {
        for nc in [
            Namecheap::new("u".into(), "k".into()),
            Namecheap::with_base_url("u".into(), "k".into(), "http://localhost".into()),
        ] {
            assert!(
                nc.client_ip().parse::<std::net::IpAddr>().is_ok(),
                "ClientIp must always be a valid address, got {:?}",
                nc.client_ip()
            );
        }
    }

    #[test]
    fn test_parse_domains_from_xml() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK">
  <CommandResponse Type="namecheap.domains.getList">
    <DomainGetListResult>
      <Domain Name="example.com" User="testuser" Created="01/01/2020" Expires="01/01/2025" IsExpired="false" IsLocked="false" AutoRenew="false" WhoisGuard="ENABLED" IsPremium="false" IsOurDNS="true"/>
      <Domain Name="another.org" User="testuser" Created="02/01/2020" Expires="02/01/2025" IsExpired="false" IsLocked="false" AutoRenew="true" WhoisGuard="ENABLED" IsPremium="false" IsOurDNS="true"/>
    </DomainGetListResult>
  </CommandResponse>
</ApiResponse>"#;
        let domains = parse_domains_from_xml(xml);
        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0], "example.com");
        assert_eq!(domains[1], "another.org");
    }

    /// Namecheap has no TLSA record type, so `create_record` rejects a TLSA up
    /// front (before any API call) with a clear, catchable reason — the managed
    /// TLSA reconcile swallows it non-fatally.
    #[tokio::test]
    async fn create_tlsa_is_rejected_without_a_network_call() {
        let nc = Namecheap::with_base_url(
            "user".into(),
            "key".into(),
            // Unroutable base URL — the guard must return before touching it.
            "http://127.0.0.1:1/xml.response".into(),
        );
        let rec = DnsRecord {
            record_type: "TLSA".into(),
            name: "_25._tcp.mail.example.com".into(),
            value: "3 1 1 abc123".into(),
            ttl: 3600,
            priority: None,
        };
        let err = nc
            .create_record(&reqwest::Client::new(), "example.com", &rec)
            .await
            .expect_err("TLSA is unsupported on Namecheap");
        assert!(
            err.to_string().contains("TLSA"),
            "the error names TLSA: {err}"
        );
    }
}
