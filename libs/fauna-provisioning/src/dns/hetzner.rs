use serde::Deserialize;

use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::{ProvisionError, ensure_success};

// Hetzner DNS now lives in the **Hetzner Cloud API** (RRset model). The old
// standalone `dns.hetzner.com/api/v1` (custom `Auth-API-Token` header) went
// read-only on 2026-05-20 and is being shut down. The Cloud API uses Bearer
// auth and groups records into RRsets keyed by `(name, type)`; we map our
// value-based seam onto the add/remove-individual-record convenience actions
// (not whole-RRset replace) so create/find/delete stay value-scoped.
const API_BASE: &str = "https://api.hetzner.cloud/v1";

/// Hetzner DNS provider (Hetzner Cloud API).
pub struct HetznerDns {
    pub token: String,
    api_base: String,
}

impl HetznerDns {
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, API_BASE.to_string())
    }
    /// Test-only constructor that lets the conformance harness point the
    /// adapter at a wiremock server.
    pub fn with_base_url(token: String, api_base: String) -> Self {
        Self { token, api_base }
    }

    /// The wire value for a record. Hetzner Cloud RRset record values are in
    /// zone-file format, so MX priority is encoded into the value
    /// (`"10 mail.example.com"`) rather than carried as a separate field —
    /// matching Gandi and preserving the orchestrator's value-equality
    /// idempotency check. **TXT** values are additionally wrapped in the zone-
    /// file double-quoting Hetzner Cloud requires (`Decode error: TXT records
    /// must be fully escaped with double quotes` otherwise) — see
    /// [`Self::to_zone_file_txt`].
    fn wire_value(record: &DnsRecord) -> String {
        if record.record_type == "TXT" {
            return Self::to_zone_file_txt(&record.value);
        }
        match record.priority {
            Some(priority) => format!("{priority} {}", record.value),
            None => record.value.clone(),
        }
    }

    /// Wrap a bare TXT value in the zone-file quoting Hetzner Cloud DNS requires:
    /// one or more `"…"` character-strings. RFC 1035 caps each character-string
    /// at 255 octets, so long values (e.g. a DKIM public key) are split into
    /// ≤255-byte quoted chunks joined by spaces. The seam value is the bare
    /// string (`to_provider_record` strips the shared builder's quotes for the
    /// providers that want it bare), so re-quote it here. Idempotent — an
    /// already-quoted value passes through unchanged.
    fn to_zone_file_txt(value: &str) -> String {
        if value.starts_with('"') {
            return value.to_string();
        }
        const MAX: usize = 255;
        if value.len() <= MAX {
            return format!("\"{value}\"");
        }
        value
            .as_bytes()
            .chunks(MAX)
            .map(|c| format!("\"{}\"", String::from_utf8_lossy(c)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Inverse of [`Self::to_zone_file_txt`]: unwrap Hetzner's zone-file TXT
    /// value back to the bare seam string by concatenating the contents of each
    /// `"…"` character-string (`"a" "b"` → `ab`). Used on read so the
    /// orchestrator's value-equality idempotency compares bare-vs-bare. A value
    /// that isn't quoted passes through (defensive; the character-strings we
    /// publish never contain an embedded `"`).
    fn from_zone_file_txt(value: &str) -> String {
        if !value.trim_start().starts_with('"') {
            return value.to_string();
        }
        value
            .split('"')
            .enumerate()
            .filter(|(i, _)| i % 2 == 1)
            .map(|(_, seg)| seg)
            .collect()
    }

    /// Fetch the RRsets matching `(name, record_type)` in a zone. An empty
    /// `Vec` means the RRset does not exist yet (no records of that name+type).
    /// Shared by `find_records` and `create_record`.
    async fn fetch_rrsets(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<HetznerRrset>, ProvisionError> {
        let resp = client
            .get(format!("{}/zones/{zone_id}/rrsets", self.api_base))
            .bearer_auth(&self.token)
            .query(&[("name", name), ("type", record_type)])
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: RrsetsResponse = resp.json().await?;
        Ok(data.rrsets)
    }
}

#[derive(Deserialize)]
struct ZonesResponse {
    zones: Vec<HetznerZone>,
}

#[derive(Deserialize)]
struct HetznerZone {
    // Cloud-API zone ids are integers; paths accept id-or-name as a string.
    id: i64,
    name: String,
}

#[derive(Deserialize)]
struct RrsetsResponse {
    #[serde(default)]
    rrsets: Vec<HetznerRrset>,
}

#[derive(Deserialize)]
struct HetznerRrset {
    // Per-RRset TTL (shared by all its records); null = inherit zone default.
    #[serde(default)]
    ttl: Option<u32>,
    #[serde(default)]
    records: Vec<HetznerRecordValue>,
}

#[derive(Deserialize)]
struct HetznerRecordValue {
    #[serde(default)]
    value: String,
}

impl DnsProvider for HetznerDns {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        let resp = client
            .get(format!("{}/zones?per_page=50", self.api_base))
            .bearer_auth(&self.token)
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: ZonesResponse = resp.json().await?;

        Ok(data
            .zones
            .into_iter()
            .map(|z| DnsZone {
                id: z.id.to_string(),
                name: z.name,
            })
            .collect())
    }

    async fn create_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        record: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        let value = Self::wire_value(record);

        // Look at the existing RRset to decide create-vs-add and to stay
        // idempotent (the orchestrator also pre-checks, but a same-value
        // create here must be a no-op rather than a duplicate).
        let existing = self
            .fetch_rrsets(client, zone_id, &record.name, &record.record_type)
            .await
            .unwrap_or_default();
        // Bool predicates only (no borrow-returning closure such as
        // `.map(|r| r.value.as_str())`): the latter trips "FnOnce is not
        // general enough" once this async fn is captured into a `Send`
        // `tokio::spawn` by the provisioning orchestrator's caller.
        let already_present = existing
            .iter()
            .any(|rs| rs.records.iter().any(|r| r.value == value));
        if already_present {
            return Ok(());
        }
        let rrset_exists = existing.iter().any(|rs| !rs.records.is_empty());

        let resp = if rrset_exists {
            // RRset already has other values → add this one to it.
            client
                .post(format!(
                    "{}/zones/{zone_id}/rrsets/{}/{}/actions/add_records",
                    self.api_base, record.name, record.record_type
                ))
                .bearer_auth(&self.token)
                .json(&serde_json::json!({
                    "records": [{ "value": value }],
                    "ttl": record.ttl,
                }))
                .send()
                .await?
        } else {
            // RRset does not exist yet → create it with this record.
            client
                .post(format!("{}/zones/{zone_id}/rrsets", self.api_base))
                .bearer_auth(&self.token)
                .json(&serde_json::json!({
                    "name": record.name,
                    "type": record.record_type,
                    "ttl": record.ttl,
                    "records": [{ "value": value }],
                }))
                .send()
                .await?
        };

        ensure_success(resp).await?;
        Ok(())
    }

    async fn find_records(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        let rrsets = self
            .fetch_rrsets(client, zone_id, name, record_type)
            .await?;

        // Flatten the matched RRsets' records into one DnsRecord per value.
        // name/type come from the request (the query already filtered to
        // them); value comes from the RRset, so the orchestrator's
        // value-equality idempotency check sees the same shape we write.
        Ok(rrsets
            .into_iter()
            .flat_map(|rs| {
                let ttl = rs.ttl.unwrap_or(0);
                rs.records.into_iter().map(move |r| DnsRecord {
                    record_type: record_type.to_string(),
                    name: name.to_string(),
                    // TXT comes back zone-file-quoted; unwrap it so the value
                    // matches the bare seam form the orchestrator compares.
                    value: if record_type == "TXT" {
                        Self::from_zone_file_txt(&r.value)
                    } else {
                        r.value
                    },
                    ttl,
                    priority: None,
                })
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
        // Remove a single record value from its RRset via the Cloud API's
        // `remove_records` action (value-scoped, not whole-RRset replace), so a
        // `_acme-challenge` teardown leaves any sibling records of the same
        // (name, type) intact. The caller passes the wire value to match (MX
        // priority already encoded in it, mirroring `create_record`/`wire_value`
        // + the other providers' value-based delete). A 2xx is success; the
        // Cloud API tolerates removing an absent value, so this stays idempotent.
        // TXT is stored zone-file-quoted, so match it in that form (else the
        // remove is a silent no-op against a `"…"`-wrapped stored value).
        let value = if record_type == "TXT" {
            Self::to_zone_file_txt(value)
        } else {
            value.to_string()
        };
        let resp = client
            .post(format!(
                "{}/zones/{zone_id}/rrsets/{name}/{record_type}/actions/remove_records",
                self.api_base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "records": [{ "value": value }] }))
            .send()
            .await?;

        ensure_success(resp).await?;
        Ok(())
    }

    fn base_url(&self) -> &str {
        &self.api_base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zones_response_with_integer_ids() {
        let json = r#"{
            "zones": [
                {"id": 42, "name": "example.com"},
                {"id": 43, "name": "another.org"}
            ],
            "meta": {"pagination": {"page": 1}}
        }"#;
        let parsed: ZonesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.zones.len(), 2);
        assert_eq!(parsed.zones[0].id, 42);
        assert_eq!(parsed.zones[0].name, "example.com");
    }

    #[test]
    fn parses_rrsets_response() {
        let json = r#"{
            "rrsets": [
                {
                    "id": "@/TXT",
                    "name": "@",
                    "type": "TXT",
                    "ttl": 300,
                    "records": [
                        {"value": "v=spf1 -all"},
                        {"value": "keep", "comment": "second value"}
                    ]
                }
            ]
        }"#;
        let parsed: RrsetsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.rrsets.len(), 1);
        assert_eq!(parsed.rrsets[0].ttl, Some(300));
        assert_eq!(parsed.rrsets[0].records.len(), 2);
        assert_eq!(parsed.rrsets[0].records[0].value, "v=spf1 -all");
    }

    #[test]
    fn empty_rrsets_response_means_no_records() {
        let parsed: RrsetsResponse = serde_json::from_str(r#"{"rrsets": []}"#).unwrap();
        assert!(parsed.rrsets.is_empty());
    }

    #[test]
    fn wire_value_encodes_mx_priority() {
        let mx = DnsRecord {
            record_type: "MX".into(),
            name: "@".into(),
            value: "mail.example.com".into(),
            ttl: 300,
            priority: Some(10),
        };
        assert_eq!(HetznerDns::wire_value(&mx), "10 mail.example.com");

        let txt = DnsRecord {
            record_type: "TXT".into(),
            name: "@".into(),
            value: "v=spf1 -all".into(),
            ttl: 300,
            priority: None,
        };
        // TXT is zone-file-quoted for Hetzner Cloud (see txt_zone_file_quoting_round_trips).
        assert_eq!(HetznerDns::wire_value(&txt), "\"v=spf1 -all\"");
    }

    /// TLSA (DANE — the floor-MX pin) has no priority, so the `<usage> <selector>
    /// <matching> <hex>` RDATA passes through verbatim as the RRset value.
    #[test]
    fn wire_value_passes_tlsa_verbatim() {
        let tlsa = DnsRecord {
            record_type: "TLSA".into(),
            name: "_25._tcp.mail.example.com".into(),
            value: "3 1 1 abcdef0123456789".into(),
            ttl: 3600,
            priority: None,
        };
        assert_eq!(HetznerDns::wire_value(&tlsa), "3 1 1 abcdef0123456789");
    }

    /// TXT must be zone-file-quoted for the Cloud API (the live `422 "TXT records
    /// must be fully escaped with double quotes"` fix), round-tripping back to the
    /// bare seam value, and long values (DKIM) split into ≤255-octet chunks.
    #[test]
    fn txt_zone_file_quoting_round_trips() {
        // Short TXT (SPF/DMARC): a single quoted character-string.
        assert_eq!(
            HetznerDns::to_zone_file_txt("v=spf1 mx ~all"),
            "\"v=spf1 mx ~all\""
        );
        assert_eq!(
            HetznerDns::from_zone_file_txt("\"v=spf1 mx ~all\""),
            "v=spf1 mx ~all"
        );

        // wire_value quotes TXT (and only TXT — MX/A are unaffected).
        let spf = DnsRecord {
            record_type: "TXT".into(),
            name: "@".into(),
            value: "v=spf1 mx ~all".into(),
            ttl: 3600,
            priority: None,
        };
        assert_eq!(HetznerDns::wire_value(&spf), "\"v=spf1 mx ~all\"");

        // Already-quoted input is not double-quoted; a bare/unquoted read passes through.
        assert_eq!(HetznerDns::to_zone_file_txt("\"x\""), "\"x\"");
        assert_eq!(HetznerDns::from_zone_file_txt("1.2.3.4"), "1.2.3.4");

        // Long TXT (DKIM key > 255): ≤255-octet quoted chunks joined by spaces,
        // reassembled by concatenating the segments.
        let long = "k".repeat(600);
        let quoted = HetznerDns::to_zone_file_txt(&long);
        assert_eq!(
            quoted.matches('"').count(),
            6,
            "3 chunks × 2 quotes: {quoted}"
        );
        assert!(quoted.split(' ').all(|seg| seg.len() <= 257)); // 255 + 2 quotes
        assert_eq!(HetznerDns::from_zone_file_txt(&quoted), long);
    }
}
